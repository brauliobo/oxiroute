use std::{
    collections::BTreeSet,
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{certbot_reconcile::PublicationGate, CertbotReconciler};

const MAX_RECONCILERS: usize = 256;
const MIN_RESCAN_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CertbotWatcherConfig {
    pub rescan_interval: Duration,
    pub event_debounce: Duration,
    pub event_max_delay: Duration,
}

impl Default for CertbotWatcherConfig {
    fn default() -> Self {
        Self {
            rescan_interval: Duration::from_secs(30),
            event_debounce: Duration::from_millis(100),
            event_max_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CertbotWatcherStatus {
    pub running: bool,
    pub degraded: bool,
    pub coalesced_events: u64,
    pub ignored_access_events: u64,
    pub backend_errors: u64,
    pub watch_recoveries: u64,
    pub watch_refreshes: u64,
    pub rescans: u64,
    pub periodic_rescans: u64,
    pub reconciliation_failures: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CertbotWatcherError {
    #[error("Certbot watcher requires at least one identity")]
    NoIdentities,
    #[error("Certbot watcher supports at most {MAX_RECONCILERS} identities, got {count}")]
    TooManyIdentities { count: usize },
    #[error(
        "Certbot watcher periodic rescan interval must be at least {} second",
        MIN_RESCAN_INTERVAL.as_secs()
    )]
    RescanIntervalTooShort,
    #[error("Certbot watcher debounce must be nonzero and no greater than max delay")]
    InvalidEventDebounce,
    #[error("Certbot watcher event max delay must be no greater than periodic rescan interval")]
    InvalidEventMaxDelay,
    #[error("failed to resolve Certbot watcher directory `{path}`")]
    Directory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot watcher path `{path}` is not a directory")]
    PathNotDirectory { path: PathBuf },
    #[error("failed to create the Certbot filesystem watcher")]
    Create {
        #[source]
        source: notify::Error,
    },
    #[error("failed to watch Certbot directory `{path}`")]
    Watch {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
    #[error("failed to start the Certbot watcher supervisor thread")]
    Thread {
        #[source]
        source: io::Error,
    },
}

#[derive(Default)]
struct WatcherState {
    running: AtomicBool,
    backend_degraded: AtomicBool,
    reconciliation_degraded: AtomicBool,
    coalesced_events: AtomicU64,
    ignored_access_events: AtomicU64,
    backend_errors: AtomicU64,
    watch_recoveries: AtomicU64,
    watch_refreshes: AtomicU64,
    rescans: AtomicU64,
    periodic_rescans: AtomicU64,
    reconciliation_failures: AtomicU64,
}

impl WatcherState {
    fn snapshot(&self) -> CertbotWatcherStatus {
        CertbotWatcherStatus {
            running: self.running.load(Ordering::Acquire),
            degraded: self.backend_degraded.load(Ordering::Acquire)
                || self.reconciliation_degraded.load(Ordering::Acquire),
            coalesced_events: self.coalesced_events.load(Ordering::Relaxed),
            ignored_access_events: self.ignored_access_events.load(Ordering::Relaxed),
            backend_errors: self.backend_errors.load(Ordering::Relaxed),
            watch_recoveries: self.watch_recoveries.load(Ordering::Relaxed),
            watch_refreshes: self.watch_refreshes.load(Ordering::Relaxed),
            rescans: self.rescans.load(Ordering::Relaxed),
            periodic_rescans: self.periodic_rescans.load(Ordering::Relaxed),
            reconciliation_failures: self.reconciliation_failures.load(Ordering::Relaxed),
        }
    }

    fn mark_backend_degraded(&self) {
        self.backend_degraded.store(true, Ordering::Release);
        self.backend_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_backend_recovered(&self) {
        if self.backend_degraded.swap(false, Ordering::AcqRel) {
            self.watch_recoveries.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn set_reconciliation_degraded(&self, degraded: bool) {
        self.reconciliation_degraded
            .store(degraded, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct CertbotWatcherMonitor {
    state: Arc<WatcherState>,
}

impl CertbotWatcherMonitor {
    #[must_use]
    pub fn status(&self) -> CertbotWatcherStatus {
        self.state.snapshot()
    }
}

impl fmt::Debug for CertbotWatcherMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertbotWatcherMonitor")
            .field("status", &self.status())
            .finish()
    }
}

#[derive(Clone)]
struct WakeQueue {
    sender: SyncSender<()>,
    state: Arc<WatcherState>,
}

impl WakeQueue {
    fn new() -> (Self, Receiver<()>, Arc<WatcherState>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(WatcherState::default());
        (
            Self {
                sender,
                state: Arc::clone(&state),
            },
            receiver,
            state,
        )
    }

    fn event(&self) {
        match self.sender.try_send(()) {
            Ok(()) | Err(TrySendError::Disconnected(())) => {}
            Err(TrySendError::Full(())) => {
                self.state.coalesced_events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn ignored_access(&self) {
        self.state
            .ignored_access_events
            .fetch_add(1, Ordering::Relaxed);
    }

    fn backend_error(&self) {
        self.state.mark_backend_degraded();
        self.event();
    }
}

pub struct CertbotWatcherSupervisor {
    gate: Arc<PublicationGate>,
    wake: WakeQueue,
    state: Arc<WatcherState>,
    worker: Option<JoinHandle<()>>,
}

impl CertbotWatcherSupervisor {
    pub(crate) fn check_if_configured(
        reconcilers: &[Arc<CertbotReconciler>],
        config: CertbotWatcherConfig,
    ) -> Result<(), CertbotWatcherError> {
        if reconcilers.is_empty() {
            return Ok(());
        }
        validate_configuration(reconcilers.len(), config)?;
        let (wake, _receiver, _state) = WakeQueue::new();
        let _watcher = WatcherBackend::new(reconcilers, &wake)?;
        Ok(())
    }

    /// Starts one watcher when at least one Certbot identity is configured.
    ///
    /// # Errors
    ///
    /// Returns an error when watcher configuration, directory setup, or thread startup fails.
    pub fn start_if_configured(
        reconcilers: Vec<Arc<CertbotReconciler>>,
        config: CertbotWatcherConfig,
    ) -> Result<Option<Self>, CertbotWatcherError> {
        if reconcilers.is_empty() {
            Ok(None)
        } else {
            Self::start(reconcilers, config).map(Some)
        }
    }

    /// Starts a bounded, debounced watcher plus periodic full reconciliation.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration is invalid, a directory cannot be resolved or watched,
    /// the notify backend cannot start, or the worker thread cannot be created.
    pub fn start(
        reconcilers: Vec<Arc<CertbotReconciler>>,
        config: CertbotWatcherConfig,
    ) -> Result<Self, CertbotWatcherError> {
        Self::start_inner(reconcilers, config)
    }

    fn start_inner(
        reconcilers: Vec<Arc<CertbotReconciler>>,
        config: CertbotWatcherConfig,
    ) -> Result<Self, CertbotWatcherError> {
        validate_configuration(reconcilers.len(), config)?;

        let (wake, receiver, state) = WakeQueue::new();
        let watcher = WatcherBackend::new(&reconcilers, &wake)?;
        let gate = Arc::new(PublicationGate::new());
        let worker_gate = Arc::clone(&gate);
        let worker_state = Arc::clone(&state);
        let worker_wake = wake.clone();
        state.running.store(true, Ordering::Release);
        let worker = thread::Builder::new()
            .name("certbot-watcher".into())
            .spawn(move || {
                let _running = RunningGuard(Arc::clone(&worker_state));
                run_worker(
                    &receiver,
                    &reconcilers,
                    config,
                    &worker_gate,
                    &worker_state,
                    &worker_wake,
                    watcher,
                );
            })
            .map_err(|source| {
                state.running.store(false, Ordering::Release);
                CertbotWatcherError::Thread { source }
            })?;
        // Reconcile once after all watches are installed. This closes the preparation-to-watcher
        // startup window without waiting for the periodic backstop.
        wake.event();

        Ok(Self {
            gate,
            wake,
            state,
            worker: Some(worker),
        })
    }

    #[must_use]
    pub fn status(&self) -> CertbotWatcherStatus {
        self.state.snapshot()
    }

    #[must_use]
    pub fn monitor(&self) -> CertbotWatcherMonitor {
        CertbotWatcherMonitor {
            state: Arc::clone(&self.state),
        }
    }

    /// Stops publication, wakes the worker, and waits for its current local-filesystem work.
    ///
    /// Local filesystem operations are synchronous and are not unsafely cancelled. A blocked local
    /// filesystem can therefore delay this call after the main process has completed bounded
    /// service-runtime shutdown.
    pub fn shutdown(&mut self) {
        self.gate.stop();
        self.wake.event();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.state.running.store(false, Ordering::Release);
    }
}

struct RunningGuard(Arc<WatcherState>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

impl fmt::Debug for CertbotWatcherSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertbotWatcherSupervisor")
            .field("status", &self.status())
            .field("running", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for CertbotWatcherSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct WatcherBackend {
    _watcher: RecommendedWatcher,
}

impl WatcherBackend {
    fn new(
        reconcilers: &[Arc<CertbotReconciler>],
        wake: &WakeQueue,
    ) -> Result<Self, CertbotWatcherError> {
        let callback_wake = wake.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            handle_notify_event(&callback_wake, event);
        })
        .map_err(|source| CertbotWatcherError::Create { source })?;
        for path in watch_paths(reconcilers)? {
            watcher
                .watch(&path, RecursiveMode::NonRecursive)
                .map_err(|source| CertbotWatcherError::Watch { path, source })?;
        }
        Ok(Self { _watcher: watcher })
    }

    fn rebuild(
        &mut self,
        reconcilers: &[Arc<CertbotReconciler>],
        wake: &WakeQueue,
    ) -> Result<(), CertbotWatcherError> {
        *self = Self::new(reconcilers, wake)?;
        Ok(())
    }
}

impl fmt::Debug for WatcherBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatcherBackend")
            .finish_non_exhaustive()
    }
}

fn validate_configuration(
    reconciler_count: usize,
    config: CertbotWatcherConfig,
) -> Result<(), CertbotWatcherError> {
    if reconciler_count == 0 {
        return Err(CertbotWatcherError::NoIdentities);
    }
    if reconciler_count > MAX_RECONCILERS {
        return Err(CertbotWatcherError::TooManyIdentities {
            count: reconciler_count,
        });
    }
    if config.rescan_interval < MIN_RESCAN_INTERVAL {
        return Err(CertbotWatcherError::RescanIntervalTooShort);
    }
    if config.event_debounce.is_zero() || config.event_debounce > config.event_max_delay {
        return Err(CertbotWatcherError::InvalidEventDebounce);
    }
    if config.event_max_delay > config.rescan_interval {
        return Err(CertbotWatcherError::InvalidEventMaxDelay);
    }
    Ok(())
}

fn watch_paths(
    reconcilers: &[Arc<CertbotReconciler>],
) -> Result<BTreeSet<PathBuf>, CertbotWatcherError> {
    let mut paths = BTreeSet::new();
    for reconciler in reconcilers {
        for configured in [
            reconciler.lineage().live_directory_path(),
            reconciler.lineage().archive_directory_path(),
        ] {
            let parent = configured
                .parent()
                .ok_or_else(|| CertbotWatcherError::Directory {
                    path: configured.into(),
                    source: io::Error::new(io::ErrorKind::InvalidInput, "directory has no parent"),
                })?;
            paths.insert(canonical_watch_path(parent)?);
            paths.insert(canonical_watch_path(configured)?);
        }
    }
    Ok(paths)
}

fn canonical_watch_path(path: &Path) -> Result<PathBuf, CertbotWatcherError> {
    let canonical =
        std::fs::canonicalize(path).map_err(|source| CertbotWatcherError::Directory {
            path: path.into(),
            source,
        })?;
    if !canonical.is_dir() {
        return Err(CertbotWatcherError::PathNotDirectory { path: canonical });
    }
    Ok(canonical)
}

fn handle_notify_event(wake: &WakeQueue, event: notify::Result<notify::Event>) {
    match event {
        Ok(event) if event.need_rescan() => wake.backend_error(),
        Ok(event) if event.kind.is_access() => wake.ignored_access(),
        Ok(_event) => wake.event(),
        Err(_error) => wake.backend_error(),
    }
}

fn run_worker(
    receiver: &Receiver<()>,
    reconcilers: &[Arc<CertbotReconciler>],
    config: CertbotWatcherConfig,
    gate: &PublicationGate,
    state: &WatcherState,
    wake: &WakeQueue,
    mut watcher: WatcherBackend,
) {
    loop {
        let periodic = match receiver.recv_timeout(config.rescan_interval) {
            Ok(()) => {
                if !debounce_events(receiver, gate, config) {
                    return;
                }
                false
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if gate.is_stopped() {
            return;
        }
        state.rescans.fetch_add(1, Ordering::Relaxed);
        if periodic {
            state.periodic_rescans.fetch_add(1, Ordering::Relaxed);
        }
        let mut reconciliation_degraded = false;
        for reconciler in reconcilers {
            if gate.is_stopped() {
                return;
            }
            match reconciler.reconcile_while_running(gate) {
                Ok(Some(_outcome)) => {}
                Ok(None) => return,
                Err(error) => {
                    log::warn!("Certbot reconciliation failed: {error}");
                    reconciliation_degraded = true;
                    state
                        .reconciliation_failures
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        state.set_reconciliation_degraded(reconciliation_degraded);
        match watcher.rebuild(reconcilers, wake) {
            Ok(()) => {
                state.watch_refreshes.fetch_add(1, Ordering::Relaxed);
                state.mark_backend_recovered();
            }
            Err(error) => {
                log::error!("failed to refresh Certbot filesystem watches: {error}");
                state.mark_backend_degraded();
            }
        }
    }
}

fn debounce_events(
    receiver: &Receiver<()>,
    gate: &PublicationGate,
    config: CertbotWatcherConfig,
) -> bool {
    let mut window = DebounceWindow::new(Instant::now());
    loop {
        if gate.is_stopped() {
            return false;
        }
        let now = Instant::now();
        let deadline = window.deadline(config.event_debounce, config.event_max_delay);
        if now >= deadline {
            return true;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok(()) => window.note_event(Instant::now()),
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

struct DebounceWindow {
    first_event: Instant,
    last_event: Instant,
}

impl DebounceWindow {
    fn new(now: Instant) -> Self {
        Self {
            first_event: now,
            last_event: now,
        }
    }

    fn note_event(&mut self, now: Instant) {
        self.last_event = now;
    }

    fn deadline(&self, debounce: Duration, max_delay: Duration) -> Instant {
        (self.last_event + debounce).min(self.first_event + max_delay)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::TryRecvError;

    use notify::{
        event::{AccessKind, AccessMode, Flag},
        Event, EventKind,
    };

    use super::*;

    #[test]
    fn bounded_wake_queue_coalesces_events() {
        let (wake, receiver, state) = WakeQueue::new();

        wake.event();
        wake.event();
        wake.event();

        receiver.try_recv().unwrap();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(state.snapshot().coalesced_events, 2);
        assert!(!state.snapshot().degraded);
    }

    #[test]
    fn notify_access_open_and_read_events_are_ignored() {
        let (wake, receiver, state) = WakeQueue::new();
        for kind in [
            AccessKind::Read,
            AccessKind::Open(AccessMode::Read),
            AccessKind::Close(AccessMode::Read),
        ] {
            handle_notify_event(&wake, Ok(Event::new(EventKind::Access(kind))));
        }

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(state.snapshot().ignored_access_events, 3);
    }

    #[test]
    fn backend_degradation_and_recovery_are_visible() {
        let (wake, receiver, state) = WakeQueue::new();

        wake.backend_error();
        receiver.try_recv().unwrap();
        assert!(state.snapshot().degraded);

        state.mark_backend_recovered();
        let status = state.snapshot();
        assert!(!status.degraded);
        assert_eq!(status.backend_errors, 1);
        assert_eq!(status.watch_recoveries, 1);
    }

    #[test]
    fn notify_overflow_rescan_flag_marks_degradation() {
        let (wake, receiver, state) = WakeQueue::new();
        let overflow = Event::new(EventKind::Other).set_flag(Flag::Rescan);

        handle_notify_event(&wake, Ok(overflow));

        receiver.try_recv().unwrap();
        assert!(state.snapshot().degraded);
    }

    #[test]
    fn validates_identity_and_timing_bounds() {
        let valid = CertbotWatcherConfig::default();
        assert!(matches!(
            validate_configuration(0, valid),
            Err(CertbotWatcherError::NoIdentities)
        ));
        assert!(matches!(
            validate_configuration(MAX_RECONCILERS + 1, valid),
            Err(CertbotWatcherError::TooManyIdentities { .. })
        ));
        assert!(matches!(
            validate_configuration(
                1,
                CertbotWatcherConfig {
                    rescan_interval: MIN_RESCAN_INTERVAL - Duration::from_millis(1),
                    ..valid
                }
            ),
            Err(CertbotWatcherError::RescanIntervalTooShort)
        ));
        assert!(matches!(
            validate_configuration(
                1,
                CertbotWatcherConfig {
                    event_debounce: Duration::ZERO,
                    ..valid
                }
            ),
            Err(CertbotWatcherError::InvalidEventDebounce)
        ));
        assert!(matches!(
            validate_configuration(
                1,
                CertbotWatcherConfig {
                    event_max_delay: valid.rescan_interval + Duration::from_millis(1),
                    ..valid
                }
            ),
            Err(CertbotWatcherError::InvalidEventMaxDelay)
        ));
    }

    #[test]
    fn debounce_deadline_is_quiet_period_bounded_by_max_delay() {
        let start = Instant::now();
        let mut window = DebounceWindow::new(start);
        let debounce = Duration::from_millis(100);
        let max_delay = Duration::from_millis(250);
        assert_eq!(window.deadline(debounce, max_delay), start + debounce);

        window.note_event(start + Duration::from_millis(80));
        assert_eq!(
            window.deadline(debounce, max_delay),
            start + Duration::from_millis(180)
        );
        window.note_event(start + Duration::from_millis(240));
        assert_eq!(window.deadline(debounce, max_delay), start + max_delay);
    }
}
