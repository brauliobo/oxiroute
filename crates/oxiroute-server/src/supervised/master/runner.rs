#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct MasterRunner {
    pub(super) launcher_path: PathBuf,
    pub(super) executable_path: PathBuf,
}

impl MasterRunner {
    pub(super) fn launcher_available() -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::metadata(PRODUCTION_LAUNCHER).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(super) fn production() -> Result<Self, Box<dyn Error>> {
        let executable_path = std::env::current_exe()?;
        if !executable_path.is_absolute() {
            return Err(
                io::Error::other("current oxiroute executable path is not absolute").into(),
            );
        }
        Ok(Self {
            launcher_path: PathBuf::from(PRODUCTION_LAUNCHER),
            executable_path,
        })
    }

    #[cfg(test)]
    pub(super) fn new_for_test(
        launcher_path: impl Into<PathBuf>,
        executable_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            launcher_path: launcher_path.into(),
            executable_path: executable_path.into(),
        }
    }

    pub(super) fn run_loaded(
        &self,
        coordinator: &CanonicalConfigCoordinator,
        document: &ResolvedConfigDocument,
    ) -> Result<(), Box<dyn Error>> {
        let config = &document.validated_config;
        oxiroute_server::apply_log_filter(config.as_draft().log_filter.as_deref());
        let reservations = ListenerReservations::prepare(config, None)?;
        let listeners = reservations.clone().into_stable_listeners(config)?;
        let mut book = ListenerBook::new(reservations, listeners.manifest().clone());
        let mut reports = ConfigReports::default();
        let token = generate_instance_token()?;
        let (instance_id, identity) = worker_identity(token)?;
        let active = self.build_launch_document(
            coordinator.canonical_path(),
            document.validated_config.clone(),
            document.effective_revision.clone(),
            instance_id,
            identity,
            &[],
        )?;
        let catalog = SupervisedGenerationCatalog::new(active);
        let master_config = master_config()?;
        let mut factory = WorkerSpawner::new(&self.launcher_path, WORKER_HANDSHAKE_TIMEOUT)?;
        let waker = Waker::new()?;
        let mut reload = ConfigReloadMonitor::start(
            coordinator.canonical_path(),
            &document.dependencies,
            Arc::clone(&waker),
        )?;
        let signals = SignalMonitor::new(waker)?;
        let launch = Master::launch(
            master_config,
            listeners,
            &mut factory,
            worker_input(catalog.active()),
            Instant::now(),
        );
        let result = match launch {
            Ok(mut master) => self.drive(
                &mut master,
                &signals,
                coordinator,
                catalog,
                &mut factory,
                &mut reload,
                &mut book,
                &mut reports,
            ),
            Err(error) => Err(error.into()),
        };
        signals.finish(result)
    }

    pub(super) fn build_worker_command(
        &self,
        config_path: &Path,
        revision: &EffectiveRevision,
        identity: WorkerIdentity,
        kept: &[StartupOnly],
    ) -> Result<WorkerCommand, Box<dyn Error>> {
        self.build_worker_command_with_environment(
            config_path,
            revision,
            identity,
            kept,
            |key: &str| std::env::var_os(key),
        )
    }

    pub(super) fn build_launch_document(
        &self,
        config_path: &Path,
        config: ValidatedConfig,
        revision: EffectiveRevision,
        instance_id: InstanceId,
        identity: WorkerIdentity,
        kept: &[StartupOnly],
    ) -> Result<LaunchDocument, Box<dyn Error>> {
        let mut command = self.build_worker_command(config_path, &revision, identity, kept)?;
        if contains_rtmp_exec_profiles(&config) {
            command = command.require_cgroup_containment();
        }
        Ok(GenerationLaunchDocument::new(
            instance_id,
            identity.generation,
            revision,
            LaunchPayload {
                config,
                identity,
                command,
            },
        ))
    }

    pub(super) fn build_worker_command_with_environment(
        &self,
        config_path: &Path,
        revision: &EffectiveRevision,
        identity: WorkerIdentity,
        kept: &[StartupOnly],
        environment: impl Fn(&str) -> Option<OsString>,
    ) -> Result<WorkerCommand, Box<dyn Error>> {
        let mut command = WorkerCommand::new(&self.executable_path)?
            .arg(super::super::MARKER)
            .arg(identity.generation.to_string())
            .arg(encode_token(identity.instance))
            .arg(config_path)
            .arg(revision.to_string());
        if !kept.is_empty() {
            command = command.arg(serde_json::to_string(kept)?);
        }
        for key in [
            "RUST_LOG",
            "OXIROUTE_MANAGEMENT_TOKEN_FILE",
            "OXIROUTE_AUDIT_DIR",
            "OXIROUTE_AUDIT_MAX_RECORDS",
            "OXIROUTE_AUDIT_MAX_RECORD_BYTES",
            "OXIROUTE_AUDIT_MAX_FILE_BYTES",
            "OXIROUTE_AUDIT_MAX_TOTAL_BYTES",
            "OXIROUTE_AUDIT_MAX_ROTATED_FILES",
        ] {
            if let Some(value) = environment(key) {
                command = command.env(key, value);
            }
        }
        Ok(command)
    }

    #[allow(clippy::too_many_arguments)]
    fn drive(
        &self,
        master: &mut Master,
        signals: &SignalMonitor,
        coordinator: &CanonicalConfigCoordinator,
        mut catalog: GenerationCatalog,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
        book: &mut ListenerBook,
        reports: &mut ConfigReports,
    ) -> Result<(), Box<dyn Error>> {
        let result = self.drive_inner(
            master,
            signals,
            coordinator,
            &mut catalog,
            factory,
            reload,
            book,
            reports,
        );
        if result.is_err() && !matches!(master.state(), MasterState::Stopped | MasterState::Failed)
        {
            cleanup_master(master);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_inner(
        &self,
        master: &mut Master,
        signals: &SignalMonitor,
        coordinator: &CanonicalConfigCoordinator,
        catalog: &mut GenerationCatalog,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
        book: &mut ListenerBook,
        reports: &mut ConfigReports,
    ) -> Result<(), Box<dyn Error>> {
        while master.state() != MasterState::Running {
            if master.state() == MasterState::Failed {
                return Err(master_failure("startup").into());
            }
            if signals.stop.load(Ordering::Acquire) {
                return shutdown_master(master);
            }
            master.poll(Instant::now())?;
            signals.wait(master, None)?;
        }
        oxiroute_server::announce_ready(
            &catalog.active().payload().config,
            catalog.active().revision(),
        );

        loop {
            if signals.stop.load(Ordering::Acquire) || shutdown_requested_by_worker(master) {
                return shutdown_master(master);
            }
            let now = Instant::now();
            let mut due = None;
            match master.state() {
                MasterState::Running => {
                    let events = master.poll(now)?;
                    log_master_events(&events);
                    reports.observe(master, catalog, &events, book)?;
                    reports.retired.report(master, now);
                    let forced_reload = catalog.candidate().is_none() && {
                        let signalled = signals.reload.swap(false, Ordering::AcqRel);
                        let queued = reports.take_queued_reload();
                        reports.reload_requested(master) || signalled || queued
                    };
                    if catalog.candidate().is_none() {
                        if forced_reload || reload.next_trigger(now) {
                            self.reconcile(
                                master,
                                coordinator,
                                catalog,
                                factory,
                                reload,
                                (book, reports),
                            )?;
                        }
                        due = Some(reload.next_deadline());
                    }
                    due = due.into_iter().chain(reports.retired.next_due()).min();
                }
                MasterState::Failed => return Err(master_failure("runtime").into()),
                MasterState::Stopped => {
                    return Err(master_failure("unexpected early shutdown").into());
                }
                _ => {
                    let events = master.poll(now)?;
                    log_master_events(&events);
                    reports.observe(master, catalog, &events, book)?;
                    reports.retired.report(master, now);
                    let signalled = signals.reload.swap(false, Ordering::AcqRel);
                    if reports.reload_requested(master) || signalled || reload.next_trigger(now) {
                        reports.queue_reload(master, coordinator, catalog);
                    }
                    due = reports.retired.next_due();
                }
            }
            signals.wait(master, due)?;
        }
    }

    fn reconcile(
        &self,
        master: &mut Master,
        coordinator: &CanonicalConfigCoordinator,
        catalog: &mut GenerationCatalog,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
        (book, reports): (&mut ListenerBook, &mut ConfigReports),
    ) -> Result<(), Box<dyn Error>> {
        // The coordinator rereads and hashes the file, and reuses the resolved document while the
        // bytes are unchanged, so a quiet reconciliation costs one file read.
        reports.hold(master, None);
        let document = match coordinator.load_shared() {
            Ok(document) => document,
            Err(rejection) => {
                reports.reject(
                    master,
                    ConfigRejection::new(rejection.disk_revision.as_ref(), &rejection.diagnostics),
                );
                return Ok(());
            }
        };
        if let Err(error) = reload.watch_dependencies(&document.dependencies) {
            warn!("supervised master could not watch a configuration dependency: {error}");
        }
        let file = document.authored_revision.clone();
        let file_revision = document.effective_revision.clone();
        let kept = startup_only(catalog, &document);
        let reasons = startup_only_reasons(&kept);
        let applicable = if kept.is_empty() {
            Ok(Arc::clone(&document))
        } else {
            document.keeping(&kept).map(Arc::new)
        };
        let document = match applicable {
            Ok(applicable) => applicable,
            Err(rejection) => {
                reports.reject(
                    master,
                    ConfigRejection::new(rejection.disk_revision.as_ref(), &rejection.diagnostics),
                );
                return Ok(());
            }
        };
        let revision = document.effective_revision.clone();
        if let Err(error) =
            supervised_eligibility(&document.validated_config, probe_cgroup_v2().status)
        {
            let message = "the configuration cannot run under the supervising master";
            reports.reject(
                master,
                refusal(&file, "E_UNSUPPORTED_CONFIG", message, error.to_string()),
            );
            return Ok(());
        }
        reports.restart_required(master, &file_revision, reasons);
        if &revision == catalog.active().revision() {
            reports.accept(master);
            return Ok(());
        }

        // Bind every new listener before any traffic moves. A listener that cannot be bound keeps
        // the running revision in force and leaves every socket as it was.
        let new_config = &document.validated_config;
        let reservations = match ListenerReservations::prepare(new_config, Some(&book.active)) {
            Ok(reservations) => reservations,
            Err(error) => {
                let message = "the configuration's listeners cannot be bound";
                reports.reject(
                    master,
                    refusal(&file, "E_LISTENER_BIND", message, error.to_string()),
                );
                return Ok(());
            }
        };
        let listeners = match reservations.clone().into_stable_listeners(new_config) {
            Ok(listeners) => listeners,
            Err(error) => {
                let message = "the configuration's listeners cannot be handed to a worker";
                reports.reject(
                    master,
                    refusal(&file, "E_LISTENER_EXPORT", message, error.to_string()),
                );
                return Ok(());
            }
        };
        let changes = book.active_manifest.changes(listeners.manifest());
        let manifest = listeners.manifest().clone();

        let candidate =
            self.begin_candidate(catalog, coordinator, (new_config, &kept), &revision)?;
        book.begin(reservations, manifest, &changes, &revision);
        reports.candidate_file = Some(file);
        reports.accept(master);
        begin_retirement(master, catalog, reports);
        if let Err(error) =
            master.replace_with_listeners(factory, candidate, listeners, Instant::now())
        {
            book.discard();
            if master.state() != MasterState::Running {
                return Err(error.into());
            }
            warn!("supervised master could not start a configuration replacement: {error}");
            return Ok(());
        }
        Ok(())
    }

    fn begin_candidate(
        &self,
        catalog: &mut GenerationCatalog,
        coordinator: &CanonicalConfigCoordinator,
        (config, kept): (&ValidatedConfig, &[StartupOnly]),
        revision: &EffectiveRevision,
    ) -> Result<WorkerInput<WorkerCommand>, Box<dyn Error>> {
        let document = self.next_launch_document(
            catalog,
            coordinator.canonical_path(),
            (config.clone(), kept),
            revision.clone(),
        )?;
        catalog.begin_candidate(document)?;
        Ok(worker_input(catalog.candidate().expect(
            "candidate was inserted immediately before launch",
        )))
    }

    fn next_launch_document(
        &self,
        catalog: &mut GenerationCatalog,
        config_path: &Path,
        (config, kept): (ValidatedConfig, &[StartupOnly]),
        revision: EffectiveRevision,
    ) -> Result<LaunchDocument, Box<dyn Error>> {
        let generation = catalog.allocate_generation()?;
        let token = generate_instance_token()?;
        let (instance_id, identity) = replacement_worker_identity(token, generation)?;
        self.build_launch_document(config_path, config, revision, instance_id, identity, kept)
    }
}

fn begin_retirement(master: &mut Master, catalog: &GenerationCatalog, reports: &mut ConfigReports) {
    let window = retirement_window(
        &catalog.active().payload().config,
        std::env::var_os(RELOAD_DRAIN_ENV).as_deref(),
    );
    info!(
        "supervised master lets the replaced worker drain for up to {} seconds",
        window.as_secs()
    );
    reports.retired.begin(window);
    master.set_retirement_deadline(window + RETIREMENT_BACKSTOP);
}

fn log_master_events(events: &[MasterEvent]) {
    if events
        .iter()
        .any(|event| !matches!(event, MasterEvent::WorkerStatusUpdated { .. }))
    {
        info!("supervised master events: {events:?}");
    }
    for line in failure_lines(events) {
        error!("{line}");
    }
}

pub(super) fn failure_lines(events: &[MasterEvent]) -> Vec<String> {
    let failing = events.iter().any(|event| {
        matches!(
            event,
            MasterEvent::FailClosed { .. } | MasterEvent::Failed { .. }
        )
    });
    if !failing {
        return Vec::new();
    }
    events
        .iter()
        .filter_map(|event| match event {
            MasterEvent::FailClosed { phase } => Some(format!(
                "supervised active worker lost ({phase:?}): the master fails closed and exits \
                 so the service manager restarts the daemon"
            )),
            MasterEvent::WorkerExited {
                role: WorkerRole::Active,
                instance_id,
                status,
            } => Some(format!(
                "supervised active worker {} exited: {status}",
                instance_id.as_str()
            )),
            _ => None,
        })
        .collect()
}
