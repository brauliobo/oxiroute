#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct GenerationProcess {
    pub(super) generation: Arc<RuntimeGeneration>,
    pub(super) retirement: CandidateRtmpRetirementHandle,
    pub(super) shutdown: tokio::sync::watch::Sender<bool>,
    pub(super) thread: JoinHandle<()>,
}

#[derive(Clone)]
pub(super) struct CandidateRtmpRetirementHandle {
    shutdown: Arc<Mutex<Option<RtmpShutdown>>>,
}

impl CandidateRtmpRetirementHandle {
    pub(super) fn capture(generation: &RuntimeGeneration) -> Self {
        Self {
            shutdown: Arc::new(Mutex::new(Some(generation.rtmp_shutdown()))),
        }
    }

    fn initiate(&self, deadline: std::time::Instant) -> Vec<RtmpShutdown> {
        self.shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .into_iter()
            .inspect(|shutdown| shutdown.initiate(deadline))
            .collect()
    }
}

const ACME_RENEWAL_SCAN_INTERVAL: Duration = Duration::from_hours(12);
const ACME_RENEWAL_OPERATION_TIMEOUT: Duration = Duration::from_mins(10);
const GENERATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct AcmeManagedSupervisor {
    pub(super) stop: Option<mpsc::Sender<()>>,
    cancellation: Dns01Cancellation,
    pub(super) thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    probe: Option<Arc<AcmeSupervisorProbe>>,
}

impl AcmeManagedSupervisor {
    pub(super) fn start(reconcilers: Vec<Arc<AcmeManagedReconciler>>) -> io::Result<Self> {
        Self::start_internal(reconcilers, None)
    }

    pub(super) fn start_internal(
        reconcilers: Vec<Arc<AcmeManagedReconciler>>,
        #[cfg(test)] probe: Option<Arc<AcmeSupervisorProbe>>,
        #[cfg(not(test))] _probe: Option<()>,
    ) -> io::Result<Self> {
        let (stop, stop_rx) = mpsc::channel();
        let cancellation = Dns01Cancellation::new();
        let worker_cancellation = cancellation.clone();
        #[cfg(test)]
        let worker_probe = probe.clone();
        let thread = thread::Builder::new()
            .name("oxiroute-acme-renewal".into())
            .spawn(move || {
                #[cfg(test)]
                let _retained = worker_probe.as_ref().and_then(|probe| {
                    probe
                        .retained
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                });
                #[cfg(test)]
                if worker_probe
                    .as_ref()
                    .is_some_and(|probe| probe.block_worker)
                {
                    if let Some(probe) = &worker_probe {
                        probe.iterations.fetch_add(1, Ordering::Release);
                    }
                    let _ = worker_cancellation.wait_timeout(Duration::from_secs(1));
                    if let Some(probe) = &worker_probe {
                        probe.worker_cleanups.fetch_add(1, Ordering::Release);
                    }
                    return;
                }
                loop {
                    #[cfg(test)]
                    if let Some(probe) = &worker_probe {
                        probe.iterations.fetch_add(1, Ordering::Relaxed);
                    }
                    let now = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .ok()
                        .map_or(0, |duration| duration.as_secs());
                    for reconciler in &reconcilers {
                        if !reconciler.renewal_due(now) {
                            continue;
                        }
                        let certificate = reconciler.status().certificate;
                        emit_certificate("certificate_renewal", "requested", &certificate);
                        let operation = AcmeOperation::with_deadline(
                            worker_cancellation.clone(),
                            std::time::Instant::now() + ACME_RENEWAL_OPERATION_TIMEOUT,
                        );
                        match reconciler.renew_now_with_operation(operation) {
                            Ok(outcome) => {
                                emit_certificate(
                                    if outcome == oxiroute_server::AcmeManagedOutcome::Activated {
                                        "certificate_activated"
                                    } else {
                                        "certificate_renewal"
                                    },
                                    if outcome == oxiroute_server::AcmeManagedOutcome::Activated {
                                        "activated"
                                    } else {
                                        "applied"
                                    },
                                    &certificate,
                                );
                                info!(
                                    "managed ACME renewal for `{}` completed with {}",
                                    certificate,
                                    outcome.code()
                                );
                            }
                            Err(error) => {
                                emit_certificate("certificate_renewal", "failed", &certificate);
                                warn!(
                                    "managed ACME renewal for `{}` failed: {}",
                                    certificate,
                                    error.code()
                                );
                            }
                        }
                    }
                    match stop_rx.recv_timeout(acme_scan_delay(&reconcilers, now)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            cancellation,
            thread: Some(thread),
            #[cfg(test)]
            probe,
        })
    }

    pub(super) fn shutdown(&mut self) {
        self.cancellation.cancel();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
            #[cfg(test)]
            if let Some(probe) = &self.probe {
                probe.stops.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
            #[cfg(test)]
            if let Some(probe) = &self.probe {
                probe.joins.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
pub(super) struct AcmeSupervisorProbe {
    pub(super) iterations: AtomicU64,
    pub(super) stops: AtomicU64,
    pub(super) joins: AtomicU64,
    pub(super) retained: Mutex<Option<Arc<()>>>,
    pub(super) block_worker: bool,
    pub(super) worker_cleanups: AtomicU64,
}

pub(super) trait GenerationRuntimeJoin {
    fn join_runtime(self) -> io::Result<()>;
}

impl GenerationRuntimeJoin for Http3Runtime {
    fn join_runtime(self) -> io::Result<()> {
        self.join()
    }
}

impl GenerationRuntimeJoin for UdpRuntime {
    fn join_runtime(self) -> io::Result<()> {
        self.join()
    }
}

pub(super) fn join_generation_runtimes<H, U>(
    _acme_supervisor: AcmeManagedSupervisor,
    http3_runtimes: Vec<H>,
    udp_runtimes: Vec<U>,
) -> io::Result<()>
where
    H: GenerationRuntimeJoin,
    U: GenerationRuntimeJoin,
{
    for runtime in http3_runtimes {
        runtime.join_runtime()?;
    }
    for runtime in udp_runtimes {
        runtime.join_runtime()?;
    }
    Ok(())
}

impl Drop for AcmeManagedSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn acme_scan_delay(reconcilers: &[Arc<AcmeManagedReconciler>], now: u64) -> Duration {
    reconcilers
        .iter()
        .filter_map(|reconciler| {
            let next = reconciler.status().next_action_unix_seconds?;
            (next > now).then(|| Duration::from_secs(next - now))
        })
        .min()
        .unwrap_or(ACME_RENEWAL_SCAN_INTERVAL)
        .min(ACME_RENEWAL_SCAN_INTERVAL)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GenerationJoinOutcome {
    Joined,
    Panicked,
    Detached,
}

impl GenerationProcess {
    pub(super) fn start(
        generation: Arc<RuntimeGeneration>,
        coordinator: CanonicalConfigCoordinator,
        manager: &GenerationManager,
        process_shutdown: &Arc<AtomicBool>,
        deadline: std::time::Instant,
    ) -> Result<Self, Box<dyn Error>> {
        let retirement = CandidateRtmpRetirementHandle::capture(&generation);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (setup_tx, setup_rx) = mpsc::sync_channel(1);
        let thread_generation = Arc::clone(&generation);
        let thread_process_shutdown = Arc::clone(process_shutdown);
        let thread_manager = manager.clone();
        let thread = match thread::Builder::new()
            .name(format!(
                "oxiroute-generation-{}",
                &generation.revision().candidate.as_str()[..12]
            ))
            .spawn(move || {
                if let Err(error) = serve_generation(
                    &thread_generation,
                    &coordinator,
                    &thread_manager,
                    thread_process_shutdown,
                    shutdown_rx,
                    &setup_tx,
                ) {
                    let _ = setup_tx.try_send(Err(error.to_string()));
                    error!("generation runtime failed: {error}");
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                finish_candidate_generation(manager, &generation, deadline);
                return Err(error.into());
            }
        };
        loop {
            if process_shutdown.load(Ordering::Acquire) {
                let process = Self {
                    generation: Arc::clone(&generation),
                    retirement,
                    shutdown: shutdown_tx,
                    thread,
                };
                let _ = finish_candidate_generation_process(manager, process, deadline);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "generation startup was cancelled",
                )
                .into());
            }
            match setup_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(())) => {
                    return Ok(Self {
                        generation,
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    });
                }
                Ok(Err(error)) => {
                    let process = Self {
                        generation: Arc::clone(&generation),
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    };
                    let _ = finish_candidate_generation_process(manager, process, deadline);
                    return Err(io::Error::other(error).into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let process = Self {
                        generation: Arc::clone(&generation),
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    };
                    let _ = finish_candidate_generation_process(manager, process, deadline);
                    return Err(
                        io::Error::other("generation setup terminated without readiness").into(),
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() >= deadline => {
                    let process = Self {
                        generation: Arc::clone(&generation),
                        retirement,
                        shutdown: shutdown_tx,
                        thread,
                    };
                    let _ = finish_candidate_generation_process(manager, process, deadline);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "generation setup did not complete before its deadline",
                    )
                    .into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    pub(super) fn request_shutdown(&self) {
        self.generation.stop_accepting();
        let _ = self.shutdown.send(true);
    }

    pub(super) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    pub(super) fn join_until(self, deadline: std::time::Instant) -> GenerationJoinOutcome {
        self.request_shutdown();
        while !self.thread.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !self.thread.is_finished() {
            return GenerationJoinOutcome::Detached;
        }
        match self.thread.join() {
            Ok(()) => GenerationJoinOutcome::Joined,
            Err(_) => GenerationJoinOutcome::Panicked,
        }
    }
}

fn finish_generation_process(process: GenerationProcess, deadline: std::time::Instant) -> bool {
    match process.join_until(deadline) {
        GenerationJoinOutcome::Joined => true,
        GenerationJoinOutcome::Panicked => {
            error!("generation runtime thread panicked during shutdown");
            false
        }
        GenerationJoinOutcome::Detached => {
            warn!("generation runtime did not stop before the process shutdown deadline");
            true
        }
    }
}

fn finish_drained_generation_process(
    manager: &GenerationManager,
    process: GenerationProcess,
    deadline: std::time::Instant,
) -> bool {
    let retirement = process.retirement.clone();
    let recorder_shutdowns = retirement.initiate(deadline);
    let mut clean = finish_generation_process(process, deadline);
    for shutdown in recorder_shutdowns {
        clean &= shutdown.wait_until(deadline);
    }
    manager.prune_completed();
    clean
}

pub(super) fn finish_candidate_generation_process(
    manager: &GenerationManager,
    process: GenerationProcess,
    deadline: std::time::Instant,
) -> bool {
    if manager
        .active()
        .is_some_and(|active| Arc::ptr_eq(&active, &process.generation))
    {
        return shutdown_generation_processes(manager, vec![process], deadline);
    }
    finish_drained_generation_process(manager, process, deadline)
}

fn finish_candidate_generation(
    manager: &GenerationManager,
    generation: &Arc<RuntimeGeneration>,
    deadline: std::time::Instant,
) {
    if manager
        .active()
        .is_some_and(|active| Arc::ptr_eq(&active, generation))
    {
        return;
    }
    let shutdown = generation.initiate_rtmp_shutdown(deadline);
    let _ = shutdown.wait_until(deadline);
    manager.prune_completed();
}

pub(super) fn shutdown_generation_processes(
    manager: &GenerationManager,
    processes: Vec<GenerationProcess>,
    deadline: std::time::Instant,
) -> bool {
    let recorder_shutdowns = manager.begin_shutdown(deadline);
    for process in &processes {
        process.generation.stop_accepting();
    }
    while std::time::Instant::now() < deadline
        && processes
            .iter()
            .any(|process| !process.generation.drained())
    {
        thread::sleep(Duration::from_millis(10));
    }
    for process in &processes {
        process.request_shutdown();
    }
    let mut clean = true;
    for shutdown in &recorder_shutdowns {
        clean &= shutdown.wait_until(deadline);
    }
    for process in processes {
        clean &= finish_generation_process(process, deadline);
    }
    manager.prune_completed();
    clean
}

/// Finishes the retired generations that left and asks the ones past their deadline to leave.
/// Returns whether every finished generation shut down cleanly.
fn reap_retired(manager: &GenerationManager, retired: &mut Vec<GenerationProcess>) -> bool {
    let mut successful = true;
    *retired = std::mem::take(retired)
        .into_iter()
        .filter_map(|process| {
            let drained = process.generation.drained();
            if drained {
                process.request_shutdown();
            }
            if process.is_finished() {
                let deadline = std::time::Instant::now() + GENERATION_SHUTDOWN_TIMEOUT;
                successful &= if drained {
                    finish_drained_generation_process(manager, process, deadline)
                } else {
                    finish_generation_process(process, deadline)
                };
                None
            } else {
                Some(process)
            }
        })
        .collect();
    for expired in manager.expire_retired() {
        if let Some(process) = retired
            .iter()
            .find(|process| Arc::ptr_eq(&process.generation, &expired))
        {
            process.request_shutdown();
        }
    }
    successful
}

pub(super) fn supervise_generations(
    manager: &GenerationManager,
    coordinator: &CanonicalConfigCoordinator,
    mut current: GenerationProcess,
    stop: &AtomicBool,
    process_shutdown: &Arc<AtomicBool>,
) -> bool {
    let mut retired = Vec::<GenerationProcess>::new();
    let mut successful = true;
    while !stop.load(Ordering::Acquire) {
        if current.is_finished() || current.generation.runtime_failed() {
            error!("active generation runtime terminated unexpectedly");
            process_shutdown.store(true, Ordering::Release);
            successful = false;
            break;
        }
        successful &= reap_retired(manager, &mut retired);
        let Some(candidate) = manager.candidate() else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        if manager.defer_for_retired_cap(&candidate) {
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        let startup_deadline = std::time::Instant::now() + GENERATION_STARTUP_TIMEOUT;
        let mut startup = match manager.begin_candidate_start(&candidate) {
            Ok(startup) => startup,
            Err(oxiroute_server::GenerationError::MutationInProgress) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => continue,
        };
        let starting_generation = match startup.claim_runtime_start() {
            Ok(generation) => generation,
            Err(error) => {
                error!("candidate generation start claim failed: {error}");
                continue;
            }
        };

        match GenerationProcess::start(
            starting_generation,
            coordinator.clone(),
            manager,
            process_shutdown,
            startup_deadline,
        ) {
            Ok(process) => {
                match wait_for_generation_ready(candidate.generation(), stop, startup_deadline) {
                    Ok(()) => match startup.activate() {
                        Ok(_) => {
                            retired.push(current);
                            current = process;
                        }
                        Err(error) => {
                            error!("candidate generation publication failed: {error}");
                            successful &= finish_candidate_generation_process(
                                manager,
                                process,
                                startup_deadline,
                            );
                        }
                    },
                    Err(error) => {
                        error!("candidate generation could not become ready: {error}");
                        successful &=
                            finish_candidate_generation_process(manager, process, startup_deadline);
                        manager.quarantine(&candidate, "runtime_start");
                    }
                }
            }
            Err(error) => {
                error!("candidate generation could not start: {error}");
                manager.quarantine(&candidate, "runtime_start");
            }
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    retired.push(current);
    let shutdown_clean = shutdown_generation_processes(manager, retired, deadline);
    successful && shutdown_clean
}

pub(super) fn wait_for_generation_ready(
    generation: &RuntimeGeneration,
    stop: &AtomicBool,
    deadline: std::time::Instant,
) -> Result<(), Box<dyn Error>> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "generation startup was cancelled",
            )
            .into());
        }
        if generation.runtime_failed() {
            return Err(io::Error::other("generation runtime failed during startup").into());
        }
        let listeners = generation.metrics().internal_listener_snapshots()?;
        if generation.runtime_started()
            && listeners.len() == generation.expected_runtime_listener_count()
            && listeners
                .iter()
                .all(|listener| listener.state == oxiroute_server::ListenerRuntimeState::Listening)
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "generation listeners did not become ready",
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
