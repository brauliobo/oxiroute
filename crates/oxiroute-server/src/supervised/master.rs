use std::{
    collections::HashSet,
    error::Error,
    ffi::OsString,
    fs::File,
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use log::{info, warn};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use oxiroute_config::ValidatedConfig;
use oxiroute_server::{
    ListenerReservations, RuntimeMode,
    config_coordinator::{
        CanonicalConfigCoordinator, ConfigLoadOutcome, EffectiveRevision, ResolvedConfigDocument,
    },
};
use oxiroute_supervision::{
    CatalogError, GenerationId, GenerationLaunchDocument, GenerationRole, InstanceId,
    SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::InstanceToken;
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterEvent, MasterState, WorkerInput,
};
use oxiroute_supervisor_process::{
    CgroupV2ProbeStatus, WorkerCommand, WorkerIdentity, WorkerSpawner, probe_cgroup_v2,
};
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    iterator::{Handle, Signals},
};
use thiserror::Error;

use super::worker;

// The package installs this fixed path. Unsupported descriptor topologies continue through the
// existing generation runtime, and development installations without the package use the same
// fallback.
const PRODUCTION_LAUNCHER: &str = "/usr/lib/oxiroute/oxiroute-worker-launcher";
const MASTER_INSTANCE_ID: &str = "oxiroute-stage-3";
const INITIAL_GENERATION: GenerationId = GenerationId(1);
const WORKER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MASTER_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
const CONFIG_RELOAD_MAX_DEBOUNCE: Duration = Duration::from_secs(2);
const CONFIG_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(30);

/// Runs the production master for one eligible canonical configuration.
pub(crate) fn run_master(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    supervised_eligibility(&document.validated_config, probe_cgroup_v2().status)?;
    MasterRunner::production()?.run_loaded(&coordinator, &document)
}

/// Runs the master when the configuration is eligible and otherwise preserves the current server.
///
/// The fixed packaged launcher is the activation gate. An eligible configuration without that
/// launcher preserves the direct generation runtime for development installations.
pub(crate) fn run_if_supported(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    let launcher_available = MasterRunner::launcher_available();
    let plan = config_plan(
        &document.validated_config,
        launcher_available,
        probe_cgroup_v2().status,
    )?;
    if std::env::var_os("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME").is_some()
        && !contains_rtmp_exec_profiles(&document.validated_config)
    {
        return crate::run(config_path);
    }
    match plan {
        ConfigPlan::Supervised => MasterRunner::production()?.run_loaded(&coordinator, &document),
        ConfigPlan::Direct if !launcher_available => {
            info!("supervised launcher is unavailable; using the direct generation runtime");
            crate::run(config_path)
        }
        ConfigPlan::Direct => crate::run(config_path),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigPlan {
    Supervised,
    Direct,
}

#[derive(Debug, Eq, Error, PartialEq)]
#[error("supervised master does not support this configuration: {reason}")]
struct UnsupportedConfig {
    reason: &'static str,
}

fn config_plan(
    config: &ValidatedConfig,
    launcher_available: bool,
    containment_status: CgroupV2ProbeStatus,
) -> Result<ConfigPlan, UnsupportedConfig> {
    let contains_exec = contains_rtmp_exec_profiles(config);
    if let Err(error) = supervised_eligibility(config, containment_status) {
        return if contains_exec {
            Err(error)
        } else {
            Ok(ConfigPlan::Direct)
        };
    }
    if launcher_available {
        Ok(ConfigPlan::Supervised)
    } else if contains_exec {
        Err(UnsupportedConfig {
            reason: "RTMP exec profiles require the supervised launcher",
        })
    } else {
        Ok(ConfigPlan::Direct)
    }
}

fn supervised_eligibility(
    config: &ValidatedConfig,
    containment_status: CgroupV2ProbeStatus,
) -> Result<(), UnsupportedConfig> {
    worker::validate_stage_one_config(config).map_err(|reason| UnsupportedConfig { reason })?;
    if contains_rtmp_exec_profiles(config) && containment_status != CgroupV2ProbeStatus::Ready {
        return Err(UnsupportedConfig {
            reason: containment_error_reason(containment_status),
        });
    }
    Ok(())
}

fn contains_rtmp_exec_profiles(config: &ValidatedConfig) -> bool {
    config
        .as_draft()
        .rtmp_services
        .iter()
        .any(|service| !service.exec_profiles.is_empty())
}

const fn containment_error_reason(status: CgroupV2ProbeStatus) -> &'static str {
    match status {
        CgroupV2ProbeStatus::Ready => "RTMP exec profile containment is ready",
        CgroupV2ProbeStatus::Unsupported => {
            "RTMP exec profiles require Linux cgroup-v2 containment"
        }
        CgroupV2ProbeStatus::Unavailable => {
            "RTMP exec profiles require an available cgroup-v2 hierarchy"
        }
        CgroupV2ProbeStatus::ReadOnly => "RTMP exec profiles require writable cgroup-v2 delegation",
        CgroupV2ProbeStatus::MissingControllers => {
            "RTMP exec profiles require the configured cgroup-v2 controllers"
        }
        CgroupV2ProbeStatus::NotDelegated => {
            "RTMP exec profiles require delegated cgroup-v2 containment"
        }
    }
}

fn load_document(
    coordinator: &CanonicalConfigCoordinator,
) -> Result<Box<ResolvedConfigDocument>, Box<dyn Error>> {
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => Ok(document),
        ConfigLoadOutcome::Rejected(rejection) => {
            let message = rejection.diagnostics.first().map_or_else(
                || "canonical configuration was rejected".to_owned(),
                |diagnostic| {
                    format!(
                        "canonical configuration was rejected ({}): {}",
                        diagnostic.code, diagnostic.message
                    )
                },
            );
            Err(io::Error::new(io::ErrorKind::InvalidData, message).into())
        }
    }
}

struct MasterRunner {
    launcher_path: PathBuf,
    executable_path: PathBuf,
}

impl MasterRunner {
    fn launcher_available() -> bool {
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

    fn production() -> Result<Self, Box<dyn Error>> {
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
    fn new_for_test(
        launcher_path: impl Into<PathBuf>,
        executable_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            launcher_path: launcher_path.into(),
            executable_path: executable_path.into(),
        }
    }

    fn run_loaded(
        &self,
        coordinator: &CanonicalConfigCoordinator,
        document: &ResolvedConfigDocument,
    ) -> Result<(), Box<dyn Error>> {
        let config = &document.validated_config;
        let reservations = ListenerReservations::prepare(config, None)?;
        let listeners = reservations.into_stable_listeners(config)?;
        let token = generate_instance_token()?;
        let (instance_id, identity) = worker_identity(token)?;
        let active = self.build_launch_document(
            coordinator.canonical_path(),
            document.validated_config.clone(),
            document.effective_revision.clone(),
            instance_id,
            identity,
        )?;
        let catalog = SupervisedGenerationCatalog::new(active);
        let master_config = master_config()?;
        let mut factory = WorkerSpawner::new(&self.launcher_path, WORKER_HANDSHAKE_TIMEOUT)?;
        let mut reload =
            ConfigReloadMonitor::start(coordinator.canonical_path(), &document.dependencies)?;
        let signals = SignalMonitor::new()?;
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
                &signals.stop,
                &signals.reload,
                coordinator,
                catalog,
                &mut factory,
                &mut reload,
            ),
            Err(error) => Err(error.into()),
        };
        signals.finish(result)
    }

    fn build_worker_command(
        &self,
        config_path: &Path,
        revision: &EffectiveRevision,
        identity: WorkerIdentity,
    ) -> Result<WorkerCommand, Box<dyn Error>> {
        self.build_worker_command_with_environment(config_path, revision, identity, |key: &str| {
            std::env::var_os(key)
        })
    }

    fn build_launch_document(
        &self,
        config_path: &Path,
        config: ValidatedConfig,
        revision: EffectiveRevision,
        instance_id: InstanceId,
        identity: WorkerIdentity,
    ) -> Result<LaunchDocument, Box<dyn Error>> {
        let mut command = self.build_worker_command(config_path, &revision, identity)?;
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

    fn build_worker_command_with_environment(
        &self,
        config_path: &Path,
        revision: &EffectiveRevision,
        identity: WorkerIdentity,
        environment: impl Fn(&str) -> Option<OsString>,
    ) -> Result<WorkerCommand, Box<dyn Error>> {
        let mut command = WorkerCommand::new(&self.executable_path)?
            .arg(super::MARKER)
            .arg(identity.generation.to_string())
            .arg(encode_token(identity.instance))
            .arg(config_path)
            .arg(revision.to_string());
        for key in [
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
        stop: &AtomicBool,
        reload_requested: &AtomicBool,
        coordinator: &CanonicalConfigCoordinator,
        mut catalog: GenerationCatalog,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
    ) -> Result<(), Box<dyn Error>> {
        let result = self.drive_inner(
            master,
            stop,
            reload_requested,
            coordinator,
            &mut catalog,
            factory,
            reload,
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
        stop: &AtomicBool,
        reload_requested: &AtomicBool,
        coordinator: &CanonicalConfigCoordinator,
        catalog: &mut GenerationCatalog,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
    ) -> Result<(), Box<dyn Error>> {
        while master.state() != MasterState::Running {
            if master.state() == MasterState::Failed {
                return Err(master_failure("startup").into());
            }
            if stop.load(Ordering::Acquire) {
                return shutdown_master(master);
            }
            master.poll(Instant::now())?;
            thread::sleep(MASTER_POLL_INTERVAL);
        }

        loop {
            if stop.load(Ordering::Acquire) {
                return shutdown_master(master);
            }
            match master.state() {
                MasterState::Running => {
                    let now = Instant::now();
                    let events = master.poll(now)?;
                    log_master_events(&events);
                    apply_catalog_events(catalog, &events)?;
                    let forced_reload = catalog.candidate().is_none()
                        && reload_requested.swap(false, Ordering::AcqRel);
                    if catalog.candidate().is_none() && (forced_reload || reload.next_trigger(now))
                    {
                        self.reconcile(master, coordinator, catalog, factory, reload)?;
                    }
                    thread::sleep(MASTER_POLL_INTERVAL);
                }
                MasterState::Failed => return Err(master_failure("runtime").into()),
                MasterState::Stopped => {
                    return Err(master_failure("unexpected early shutdown").into());
                }
                _ => {
                    let events = master.poll(Instant::now())?;
                    log_master_events(&events);
                    apply_catalog_events(catalog, &events)?;
                    thread::sleep(MASTER_POLL_INTERVAL);
                }
            }
        }
    }

    fn reconcile(
        &self,
        master: &mut Master,
        coordinator: &CanonicalConfigCoordinator,
        catalog: &mut GenerationCatalog,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
    ) -> Result<(), Box<dyn Error>> {
        let document = match coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document,
            ConfigLoadOutcome::Rejected(_) => {
                warn!("supervised master ignored a rejected configuration reload");
                return Ok(());
            }
        };
        if let Err(error) = reload.watch_dependencies(&document.dependencies) {
            warn!("supervised master could not watch a configuration dependency: {error}");
        }
        let revision = document.effective_revision.clone();
        if &revision == catalog.active().revision() {
            return Ok(());
        }
        if let Err(error) =
            supervised_eligibility(&document.validated_config, probe_cgroup_v2().status)
        {
            warn!("supervised master rejected an unsafe configuration reload: {error}");
            return Ok(());
        }
        if ListenerReservations::listener_restart_required(
            RuntimeMode::Supervised,
            &catalog.active().payload().config,
            &document.validated_config,
        ) {
            if has_restart_required_revision(catalog, &revision) {
                return Ok(());
            }
            let restart_required = self.next_launch_document(
                catalog,
                coordinator.canonical_path(),
                document.validated_config.clone(),
                revision,
            )?;
            catalog.record_restart_required(restart_required)?;
            warn!("supervised master retained a configuration reload that changes listeners");
            return Ok(());
        }

        let candidate = self.next_launch_document(
            catalog,
            coordinator.canonical_path(),
            document.validated_config.clone(),
            revision,
        )?;
        catalog.begin_candidate(candidate)?;
        let candidate = worker_input(
            catalog
                .candidate()
                .expect("candidate was inserted immediately before launch"),
        );
        if let Err(error) = master.replace(factory, candidate, Instant::now()) {
            if master.state() != MasterState::Running {
                return Err(error.into());
            }
            warn!("supervised master could not start a configuration replacement: {error}");
            return Ok(());
        }
        Ok(())
    }

    fn next_launch_document(
        &self,
        catalog: &mut GenerationCatalog,
        config_path: &Path,
        config: ValidatedConfig,
        revision: EffectiveRevision,
    ) -> Result<LaunchDocument, Box<dyn Error>> {
        let generation = catalog.allocate_generation()?;
        let token = generate_instance_token()?;
        let (instance_id, identity) = replacement_worker_identity(token, generation)?;
        self.build_launch_document(config_path, config, revision, instance_id, identity)
    }
}

fn log_master_events(events: &[MasterEvent]) {
    if events
        .iter()
        .any(|event| !matches!(event, MasterEvent::WorkerStatusUpdated { .. }))
    {
        info!("supervised master events: {events:?}");
    }
}

#[derive(Clone, Debug)]
struct LaunchPayload {
    config: ValidatedConfig,
    identity: WorkerIdentity,
    command: WorkerCommand,
}

type LaunchDocument = GenerationLaunchDocument<EffectiveRevision, LaunchPayload>;
type GenerationCatalog = SupervisedGenerationCatalog<EffectiveRevision, LaunchPayload>;

fn worker_input(document: &LaunchDocument) -> WorkerInput<WorkerCommand> {
    WorkerInput {
        instance_id: document.instance_id().clone(),
        identity: document.payload().identity,
        command: document.payload().command.clone(),
    }
}

fn has_restart_required_revision(
    catalog: &GenerationCatalog,
    revision: &EffectiveRevision,
) -> bool {
    catalog
        .restart_required()
        .is_some_and(|retained| retained.revision() == revision)
}

fn apply_catalog_events(
    catalog: &mut GenerationCatalog,
    events: &[MasterEvent],
) -> Result<(), CatalogSyncError> {
    for event in events {
        match event {
            MasterEvent::SpawnFailed { instance_id } => {
                expect_catalog_instance(catalog, GenerationRole::Candidate, instance_id)?;
                catalog.quarantine_candidate()?;
            }
            MasterEvent::ReplacementCommitted { active, retired } => {
                expect_catalog_instance(catalog, GenerationRole::Candidate, active)?;
                expect_catalog_instance(catalog, GenerationRole::Active, retired)?;
                catalog.commit_candidate()?;
            }
            MasterEvent::ReplacementCompleted { active }
            | MasterEvent::RollbackCompleted { active } => {
                expect_catalog_instance(catalog, GenerationRole::Active, active)?;
            }
            MasterEvent::RollbackStarted { candidate, .. } => {
                expect_catalog_instance(catalog, GenerationRole::Candidate, candidate)?;
                catalog.quarantine_candidate()?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn expect_catalog_instance(
    catalog: &GenerationCatalog,
    role: GenerationRole,
    actual: &InstanceId,
) -> Result<(), CatalogSyncError> {
    let expected = catalog.get(role).map(GenerationLaunchDocument::instance_id);
    if expected == Some(actual) {
        return Ok(());
    }
    Err(CatalogSyncError::UnexpectedInstance {
        role,
        expected: expected.cloned(),
        actual: actual.clone(),
    })
}

#[derive(Debug, Error)]
enum CatalogSyncError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(
        "master event referenced generation instance {actual} as {role:?}, expected {expected:?}"
    )]
    UnexpectedInstance {
        role: GenerationRole,
        expected: Option<InstanceId>,
        actual: InstanceId,
    },
}

struct ConfigReloadMonitor {
    watcher: RecommendedWatcher,
    watched_directories: HashSet<PathBuf>,
    events: Receiver<()>,
    next_reconciliation: Instant,
    first_event: Option<Instant>,
    last_event: Option<Instant>,
}

impl ConfigReloadMonitor {
    fn start(path: &Path, dependencies: &[PathBuf]) -> notify::Result<Self> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let (wake, events) = mpsc::sync_channel(1);
        let watcher_wake = wake.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let relevant = match event {
                    Err(_) => true,
                    Ok(event) => event.need_rescan() || !event.kind.is_access(),
                };
                if relevant {
                    let _ = watcher_wake.try_send(());
                }
            },
            notify::Config::default(),
        )?;
        watcher.watch(&parent, RecursiveMode::NonRecursive)?;
        let mut monitor = Self {
            watcher,
            watched_directories: HashSet::from([parent]),
            events,
            next_reconciliation: Instant::now(),
            first_event: None,
            last_event: None,
        };
        monitor.watch_dependencies(dependencies)?;
        Ok(monitor)
    }

    fn watch_dependencies(&mut self, dependencies: &[PathBuf]) -> notify::Result<()> {
        for dependency in dependencies {
            let Some(parent) = dependency
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
            else {
                continue;
            };
            let parent = parent.to_path_buf();
            if self.watched_directories.insert(parent.clone()) {
                self.watcher.watch(&parent, RecursiveMode::NonRecursive)?;
            }
        }
        Ok(())
    }

    fn next_trigger(&mut self, now: Instant) -> bool {
        if self.events.try_iter().next().is_some() {
            self.first_event.get_or_insert(now);
            self.last_event = Some(now);
        }
        if let (Some(first_event), Some(last_event)) = (self.first_event, self.last_event) {
            let quiet = now.saturating_duration_since(last_event) >= CONFIG_RELOAD_DEBOUNCE;
            let bounded = now.saturating_duration_since(first_event) >= CONFIG_RELOAD_MAX_DEBOUNCE;
            if quiet || bounded {
                self.first_event = None;
                self.last_event = None;
                self.next_reconciliation = now + CONFIG_RECONCILIATION_INTERVAL;
                return true;
            }
            return false;
        }
        let periodic = now >= self.next_reconciliation;
        if periodic {
            self.next_reconciliation = now + CONFIG_RECONCILIATION_INTERVAL;
        }
        periodic
    }
}

fn master_config() -> Result<MasterConfig, Box<dyn Error>> {
    Ok(MasterConfig::new(
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(6),
    )?)
}

fn worker_identity(token: InstanceToken) -> Result<(InstanceId, WorkerIdentity), Box<dyn Error>> {
    Ok((
        InstanceId::new(MASTER_INSTANCE_ID)?,
        WorkerIdentity {
            instance: token,
            generation: INITIAL_GENERATION,
            protocol: CONTROL_PROTOCOL_VERSION,
        },
    ))
}

fn replacement_worker_identity(
    token: InstanceToken,
    generation: GenerationId,
) -> Result<(InstanceId, WorkerIdentity), Box<dyn Error>> {
    Ok((
        InstanceId::new(&format!("{MASTER_INSTANCE_ID}-{generation}"))?,
        WorkerIdentity {
            instance: token,
            generation,
            protocol: CONTROL_PROTOCOL_VERSION,
        },
    ))
}

fn generate_instance_token() -> io::Result<InstanceToken> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(InstanceToken(bytes))
}

fn encode_token(token: InstanceToken) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(32);
    for byte in token.0 {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn shutdown_master(master: &mut Master) -> Result<(), Box<dyn Error>> {
    master.shutdown(Instant::now())?;
    loop {
        match master.state() {
            MasterState::Stopped => return Ok(()),
            MasterState::Failed => return Err(master_failure("shutdown").into()),
            _ => {
                master.poll(Instant::now())?;
                thread::sleep(MASTER_POLL_INTERVAL);
            }
        }
    }
}

fn cleanup_master(master: &mut Master) {
    if matches!(master.state(), MasterState::Stopped | MasterState::Failed) {
        return;
    }
    let _ = master.shutdown(Instant::now());
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    while !matches!(master.state(), MasterState::Stopped | MasterState::Failed)
        && Instant::now() < deadline
    {
        if master.poll(Instant::now()).is_err() {
            break;
        }
        thread::sleep(MASTER_POLL_INTERVAL);
    }
}

fn master_failure(phase: &str) -> io::Error {
    io::Error::other(format!("supervised master failed during {phase}"))
}

struct SignalMonitor {
    stop: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
    handle: Handle,
    thread: JoinHandle<()>,
}

impl SignalMonitor {
    fn new() -> Result<Self, Box<dyn Error>> {
        let stop = Arc::new(AtomicBool::new(false));
        let reload = Arc::new(AtomicBool::new(false));
        let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
        let handle = signals.handle();
        let signal_stop = Arc::clone(&stop);
        let signal_reload = Arc::clone(&reload);
        let thread = thread::Builder::new()
            .name("oxiroute-master-signals".into())
            .spawn(move || {
                for signal in signals.forever() {
                    match signal {
                        SIGTERM | SIGINT => {
                            signal_stop.store(true, Ordering::Release);
                            break;
                        }
                        SIGHUP => signal_reload.store(true, Ordering::Release),
                        _ => {}
                    }
                }
            })?;
        Ok(Self {
            stop,
            reload,
            handle,
            thread,
        })
    }

    fn finish(self, result: Result<(), Box<dyn Error>>) -> Result<(), Box<dyn Error>> {
        let Self {
            stop: _,
            reload: _,
            handle,
            thread,
        } = self;
        handle.close();
        let signal_result = thread.join();
        match (result, signal_result) {
            (Err(error), _) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(_)) => {
                Err(io::Error::other("master signal thread terminated unexpectedly").into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::PathBuf, str::FromStr as _};

    use oxiroute_config::{
        ConfigDraft, DownstreamTimeoutPolicy, Listener, ListenerBind, Management, Protocol, Stats,
    };
    use oxiroute_supervisor_master::FailurePhase;
    use serde_json::json;

    use super::*;

    fn config() -> ConfigDraft {
        serde_json::from_value(json!({
            "version": 1,
            "certificates": [{
                "name": "downstream",
                "dns_names": ["proxy.example.test"],
                "source": {
                    "type": "files",
                    "certificate_chain_path": "/tmp/oxiroute-master-test-chain.pem",
                    "private_key_path": "/tmp/oxiroute-master-test-key.pem"
                }
            }],
            "tls_profiles": [{
                "name": "h3",
                "certificates": ["downstream"],
                "default_certificate": "downstream",
                "min_version": "1.3",
                "alpn": ["h3"]
            }],
            "listeners": [],
            "http_services": [
                {
                    "name": "web",
                    "routes": [{
                        "path": {"kind": "segment_prefix", "value": "/"},
                        "action": {"type": "fixed_response", "status": 200}
                    }]
                },
                {
                    "name": "changed",
                    "routes": [{
                        "path": {"kind": "segment_prefix", "value": "/"},
                        "action": {"type": "fixed_response", "status": 200}
                    }]
                }
            ],
            "forward_proxy_services": [{
                "name": "forward",
                "enabled_versions": ["h1", "h2", "h3"],
                "tls_required": false
            }],
            "rtmp_services": [{
                "name": "live",
                "applications": [{"name": "broadcast", "live": true}]
            }],
            "upstream_pools": [{
                "name": "origin",
                "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]
            }],
            "l4_services": [{"name": "relay", "upstream_pool": "origin", "udp": {}}]
        }))
        .expect("supervised test config")
    }

    fn listener(name: &str, protocol: Protocol, tls_profile: Option<&str>) -> Listener {
        let service = match protocol {
            Protocol::Http | Protocol::Http3 => "web",
            Protocol::Rtmp => "live",
            Protocol::Tcp | Protocol::Udp => "relay",
            Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => "forward",
        };
        let bind = if matches!(
            protocol,
            Protocol::Udp | Protocol::Http3 | Protocol::ForwardHttp3
        ) {
            ListenerBind::Udp {
                address: SocketAddr::from(([127, 0, 0, 1], 8080)),
            }
        } else {
            ListenerBind::Socket {
                address: SocketAddr::from(([127, 0, 0, 1], 8080)),
            }
        };
        Listener {
            name: name.into(),
            bind,
            protocol,
            service: Some(service.into()),
            tls_profile: tls_profile.map(str::to_owned),
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }
    }

    fn validated(config: &ConfigDraft) -> ValidatedConfig {
        config.clone().validate().expect("valid supervised config")
    }

    #[test]
    fn eligibility_accepts_stream_listener_configuration() {
        let mut config = config();
        config
            .listeners
            .push(listener("http", Protocol::Http, None));
        assert_eq!(
            supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
            Ok(())
        );

        config.listeners[0] = listener("rtmp", Protocol::Rtmp, None);
        assert_eq!(
            supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
            Ok(())
        );

        config.listeners[0] = listener("forward-h3", Protocol::ForwardHttp3, Some("h3"));
        assert_eq!(
            supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
            Ok(())
        );

        config.listeners[0] = listener("udp", Protocol::Udp, None);
        assert_eq!(
            supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
            Ok(())
        );
    }

    #[test]
    fn eligibility_uses_the_worker_descriptor_limit() {
        let mut config = config();
        config.listeners = (0..=oxiroute_supervision_unix::MAX_DESCRIPTOR_COUNT)
            .map(|index| {
                let mut listener = listener(&format!("listener-{index}"), Protocol::Http, None);
                listener.bind = ListenerBind::Socket {
                    address: SocketAddr::from((
                        [127, 0, 0, 1],
                        10_000 + u16::try_from(index).expect("bounded listener index"),
                    )),
                };
                listener
            })
            .collect();
        assert!(matches!(
            supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
            Err(UnsupportedConfig {
                reason: "Stage 2 worker listener descriptor limit is 64",
            })
        ));
    }

    #[test]
    fn eligibility_accepts_authenticated_management_on_the_supervised_runtime() {
        let mut config = config();
        config.management = Some(Management {
            bind: SocketAddr::from(([127, 0, 0, 1], 9900)),
            ui_dir: None,
        });
        assert_eq!(
            supervised_eligibility(&validated(&config), CgroupV2ProbeStatus::Ready),
            Ok(())
        );
    }

    fn config_with_exec_profile() -> ConfigDraft {
        let mut config = config();
        config.rtmp_services[0].exec_profiles = vec![
            serde_json::from_value(json!({
                "name": "publisher",
                "application": "broadcast",
                "executable": "/bin/true",
                "working_directory": "/tmp"
            }))
            .expect("exec profile"),
        ];
        config
    }

    #[test]
    fn config_plan_requires_supervision_and_ready_containment_for_exec_profiles() {
        let config = validated(&config_with_exec_profile());

        assert_eq!(
            config_plan(&config, true, CgroupV2ProbeStatus::Ready),
            Ok(ConfigPlan::Supervised)
        );
        assert_eq!(
            config_plan(&config, false, CgroupV2ProbeStatus::Ready),
            Err(UnsupportedConfig {
                reason: "RTMP exec profiles require the supervised launcher"
            })
        );
        for (status, reason) in [
            (
                CgroupV2ProbeStatus::Unsupported,
                "RTMP exec profiles require Linux cgroup-v2 containment",
            ),
            (
                CgroupV2ProbeStatus::Unavailable,
                "RTMP exec profiles require an available cgroup-v2 hierarchy",
            ),
            (
                CgroupV2ProbeStatus::ReadOnly,
                "RTMP exec profiles require writable cgroup-v2 delegation",
            ),
            (
                CgroupV2ProbeStatus::NotDelegated,
                "RTMP exec profiles require delegated cgroup-v2 containment",
            ),
            (
                CgroupV2ProbeStatus::MissingControllers,
                "RTMP exec profiles require the configured cgroup-v2 controllers",
            ),
        ] {
            let error = config_plan(&config, true, status).unwrap_err();
            assert_eq!(error, UnsupportedConfig { reason });
            assert!(!error.to_string().contains("/sys/fs/cgroup"));
        }
    }

    #[test]
    fn config_plan_preserves_no_exec_direct_and_supervised_behavior() {
        let config = validated(&config());

        assert_eq!(
            config_plan(&config, false, CgroupV2ProbeStatus::Unavailable),
            Ok(ConfigPlan::Direct)
        );
        assert_eq!(
            config_plan(&config, true, CgroupV2ProbeStatus::Unavailable),
            Ok(ConfigPlan::Supervised)
        );
    }

    #[test]
    fn worker_identity_preserves_token_and_fixed_generation() {
        let token = InstanceToken([0xab; 16]);
        let (instance_id, identity) = worker_identity(token).expect("worker identity");
        assert_eq!(instance_id.as_str(), MASTER_INSTANCE_ID);
        assert_eq!(identity.instance, token);
        assert_eq!(identity.generation, INITIAL_GENERATION);
        assert_eq!(identity.protocol, CONTROL_PROTOCOL_VERSION);
    }

    #[test]
    fn replacement_identity_is_unique_and_monotonic() {
        let (_, first) = replacement_worker_identity(InstanceToken([0; 16]), GenerationId(2))
            .expect("first identity");
        let (second_id, second) =
            replacement_worker_identity(InstanceToken([1; 16]), GenerationId(3))
                .expect("second identity");

        assert_eq!(first.generation, GenerationId(2));
        assert_eq!(second.generation, GenerationId(3));
        assert_eq!(second_id.as_str(), "oxiroute-stage-3-3");
        assert_ne!(first.instance, second.instance);
    }

    fn launch_catalog() -> (MasterRunner, GenerationCatalog) {
        let runner = MasterRunner::new_for_test(
            "/test-only/oxiroute-worker-launcher",
            std::env::current_exe().expect("test executable"),
        );
        let revision = EffectiveRevision::from_str(&"a".repeat(64)).expect("active revision");
        let (instance_id, identity) =
            worker_identity(InstanceToken([0x11; 16])).expect("active identity");
        let active = runner
            .build_launch_document(
                Path::new("/etc/oxiroute/oxiroute.kdl"),
                validated(&config()),
                revision,
                instance_id,
                identity,
            )
            .expect("active launch document");
        (runner, SupervisedGenerationCatalog::new(active))
    }

    fn add_candidate(
        runner: &MasterRunner,
        catalog: &mut GenerationCatalog,
        revision_digit: char,
    ) -> InstanceId {
        let generation = catalog.allocate_generation().expect("candidate generation");
        let token_byte = u8::try_from(generation.0).expect("test generation fits token byte");
        let (instance_id, identity) =
            replacement_worker_identity(InstanceToken([token_byte; 16]), generation)
                .expect("candidate identity");
        let revision = EffectiveRevision::from_str(&revision_digit.to_string().repeat(64))
            .expect("candidate revision");
        let candidate = runner
            .build_launch_document(
                Path::new("/etc/oxiroute/oxiroute.kdl"),
                validated(&config()),
                revision,
                instance_id.clone(),
                identity,
            )
            .expect("candidate launch document");
        catalog
            .begin_candidate(candidate)
            .expect("catalog candidate");
        instance_id
    }

    #[test]
    fn worker_input_is_derived_from_the_exact_catalog_candidate() {
        let (runner, mut catalog) = launch_catalog();
        let candidate_id = add_candidate(&runner, &mut catalog, 'b');
        let document = catalog.candidate().expect("candidate document");
        let input = worker_input(document);

        assert_eq!(input.instance_id, candidate_id);
        assert_eq!(input.identity, document.payload().identity);
        assert_eq!(input.identity.generation, document.generation_id());
        assert_eq!(
            format!("{:?}", input.command),
            format!("{:?}", document.payload().command)
        );
    }

    #[test]
    fn exec_profile_launch_document_requires_cgroup_containment() {
        let runner = MasterRunner::new_for_test(
            "/test-only/oxiroute-worker-launcher",
            std::env::current_exe().expect("test executable"),
        );
        let revision = EffectiveRevision::from_str(&"a".repeat(64)).expect("revision");
        let (instance_id, identity) = worker_identity(InstanceToken([0x31; 16])).expect("identity");
        let document = runner
            .build_launch_document(
                Path::new("/etc/oxiroute/oxiroute.kdl"),
                validated(&config_with_exec_profile()),
                revision,
                instance_id,
                identity,
            )
            .expect("exec launch document");

        assert!(
            format!("{:?}", document.payload().command)
                .contains("require_cgroup_containment: true")
        );
    }

    #[test]
    fn catalog_maps_commit_and_completion_in_master_event_order() {
        let (runner, mut catalog) = launch_catalog();
        let retired = catalog.active().instance_id().clone();
        let active = add_candidate(&runner, &mut catalog, 'b');

        apply_catalog_events(
            &mut catalog,
            &[
                MasterEvent::ReplacementCommitted {
                    active: active.clone(),
                    retired: retired.clone(),
                },
                MasterEvent::ReplacementCompleted {
                    active: active.clone(),
                },
            ],
        )
        .expect("ordered replacement events");

        assert_eq!(catalog.active().instance_id(), &active);
        assert_eq!(catalog.previous().unwrap().instance_id(), &retired);
        assert!(catalog.candidate().is_none());
    }

    #[test]
    fn catalog_quarantines_rollback_and_spawn_failure_candidates() {
        let (runner, mut rollback_catalog) = launch_catalog();
        let active = rollback_catalog.active().instance_id().clone();
        let rollback = add_candidate(&runner, &mut rollback_catalog, 'b');
        apply_catalog_events(
            &mut rollback_catalog,
            &[
                MasterEvent::RollbackStarted {
                    candidate: rollback.clone(),
                    phase: FailurePhase::Activation,
                },
                MasterEvent::RollbackCompleted { active },
            ],
        )
        .expect("rollback events");
        assert_eq!(
            rollback_catalog.quarantined().unwrap().instance_id(),
            &rollback
        );
        assert!(rollback_catalog.candidate().is_none());

        let (runner, mut spawn_catalog) = launch_catalog();
        let failed = add_candidate(&runner, &mut spawn_catalog, 'c');
        apply_catalog_events(
            &mut spawn_catalog,
            &[MasterEvent::SpawnFailed {
                instance_id: failed.clone(),
            }],
        )
        .expect("spawn failure event");
        assert_eq!(spawn_catalog.quarantined().unwrap().instance_id(), &failed);
        assert!(spawn_catalog.candidate().is_none());
    }

    #[test]
    fn catalog_retains_and_deduplicates_restart_required_revision() {
        let (runner, mut catalog) = launch_catalog();
        let generation = catalog.allocate_generation().expect("restart generation");
        let (restart_id, identity) =
            replacement_worker_identity(InstanceToken([0x22; 16]), generation)
                .expect("restart identity");
        let restart = runner
            .build_launch_document(
                Path::new("/etc/oxiroute/oxiroute.kdl"),
                validated(&config()),
                EffectiveRevision::from_str(&"d".repeat(64)).expect("restart revision"),
                restart_id.clone(),
                identity,
            )
            .expect("restart launch document");
        let revision = restart.revision().clone();
        catalog
            .record_restart_required(restart)
            .expect("restart-required document");

        assert_eq!(
            catalog.restart_required().unwrap().instance_id(),
            &restart_id
        );
        assert!(has_restart_required_revision(&catalog, &revision));
        assert!(!has_restart_required_revision(
            &catalog,
            &EffectiveRevision::from_str(&"e".repeat(64)).expect("other revision")
        ));
        assert_eq!(catalog.allocate_generation(), Ok(GenerationId(3)));
    }

    #[test]
    fn replacement_requires_the_same_listener_manifest() {
        let mut active = config();
        active
            .listeners
            .push(listener("http", Protocol::Http, None));
        let mut candidate = active.clone();
        candidate.listeners[0].service = Some("changed".into());
        assert!(ListenerReservations::same_supervised_listener_topology(
            &validated(&active),
            &validated(&candidate),
        ));

        candidate.listeners[0].name = "renamed".into();
        assert!(!ListenerReservations::same_supervised_listener_topology(
            &validated(&active),
            &validated(&candidate),
        ));

        candidate = active.clone();
        candidate.listeners[0].bind = ListenerBind::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], 8081)),
        };
        assert!(!ListenerReservations::same_supervised_listener_topology(
            &validated(&active),
            &validated(&candidate),
        ));

        active = config();
        active.management = Some(Management {
            bind: SocketAddr::from(([127, 0, 0, 1], 9900)),
            ui_dir: None,
        });
        candidate = active.clone();
        candidate.management = Some(Management {
            bind: SocketAddr::from(([127, 0, 0, 1], 9901)),
            ui_dir: None,
        });
        assert!(!ListenerReservations::same_supervised_listener_topology(
            &validated(&active),
            &validated(&candidate),
        ));

        candidate = active.clone();
        candidate.stats = Some(Stats {
            binds: vec![SocketAddr::from(([127, 0, 0, 1], 8404))],
            admin_token_file: None,
            pages: Vec::new(),
        });
        assert!(!ListenerReservations::same_supervised_listener_topology(
            &validated(&active),
            &validated(&candidate),
        ));
    }

    #[test]
    fn production_runner_uses_the_fixed_launcher_and_absolute_executable() {
        let runner = MasterRunner::production().expect("production runner");
        assert_eq!(runner.launcher_path, Path::new(PRODUCTION_LAUNCHER));
        assert!(runner.executable_path.is_absolute());
    }

    #[test]
    fn worker_command_contains_metadata_but_not_launcher_configuration() {
        let executable = std::env::current_exe().expect("test executable");
        let launcher = PathBuf::from("/test-only/oxiroute-worker-launcher");
        let runner = MasterRunner::new_for_test(launcher.clone(), executable.clone());
        let revision = EffectiveRevision::from_str(&"a".repeat(64)).expect("revision");
        let (_, identity) = worker_identity(InstanceToken([0xcd; 16])).expect("identity");
        let command = runner
            .build_worker_command(Path::new("/etc/oxiroute/oxiroute.kdl"), &revision, identity)
            .expect("worker command");
        let debug = format!("{command:?}");

        assert!(debug.contains(executable.to_string_lossy().as_ref()));
        assert!(debug.contains(super::super::MARKER));
        assert!(debug.contains(", \"1\","));
        assert!(debug.contains(&"cd".repeat(16)));
        assert!(debug.contains("/etc/oxiroute/oxiroute.kdl"));
        assert!(debug.contains(&"a".repeat(64)));
        assert!(!debug.contains(launcher.to_string_lossy().as_ref()));

        let command = runner
            .build_worker_command_with_environment(
                Path::new("/etc/oxiroute/oxiroute.kdl"),
                &revision,
                identity,
                |key| match key {
                    "OXIROUTE_MANAGEMENT_TOKEN_FILE" => {
                        Some(OsString::from("/etc/oxiroute/management.token"))
                    }
                    "OXIROUTE_AUDIT_DIR" => Some(OsString::from("/var/lib/oxiroute/audit")),
                    "OXIROUTE_AUDIT_MAX_RECORDS" => Some(OsString::from("10000")),
                    "OXIROUTE_AUDIT_MAX_RECORD_BYTES" => Some(OsString::from("16384")),
                    "OXIROUTE_AUDIT_MAX_FILE_BYTES" => Some(OsString::from("1048576")),
                    "OXIROUTE_AUDIT_MAX_TOTAL_BYTES" => Some(OsString::from("8388608")),
                    "OXIROUTE_AUDIT_MAX_ROTATED_FILES" => Some(OsString::from("7")),
                    _ => None,
                },
            )
            .expect("worker environment");
        let debug = format!("{command:?}");
        for expected in [
            "/etc/oxiroute/management.token",
            "/var/lib/oxiroute/audit",
            "10000",
            "16384",
            "1048576",
            "8388608",
            "7",
        ] {
            assert!(
                debug.contains(expected),
                "worker command omitted {expected}"
            );
        }
    }
}
