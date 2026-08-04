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

use super::{certbot_reconcile::PublicationGate, FileReconciler};

pub use super::certbot_watcher::CertbotWatcherConfig as FileWatcherConfig;

const MAX_RECONCILERS: usize = 256;
const MIN_RESCAN_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileWatcherStatus {
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
pub enum FileWatcherError {
    #[error("direct-file watcher requires at least one identity")]
    NoIdentities,
    #[error("direct-file watcher supports at most {MAX_RECONCILERS} identities, got {count}")]
    TooManyIdentities { count: usize },
    #[error(
        "direct-file watcher periodic rescan interval must be at least {} second",
        MIN_RESCAN_INTERVAL.as_secs()
    )]
    RescanIntervalTooShort,
    #[error("direct-file watcher debounce must be nonzero and no greater than max delay")]
    InvalidEventDebounce,
    #[error(
        "direct-file watcher event max delay must be no greater than periodic rescan interval"
    )]
    InvalidEventMaxDelay,
    #[error("failed to resolve direct-file watcher directory `{path}`")]
    Directory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("direct-file watcher path `{path}` is not a directory")]
    PathNotDirectory { path: PathBuf },
    #[error("failed to create the direct-file filesystem watcher")]
    Create {
        #[source]
        source: notify::Error,
    },
    #[error("failed to watch direct-file directory `{path}`")]
    Watch {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
    #[error("failed to start the direct-file watcher supervisor thread")]
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
    fn snapshot(&self) -> FileWatcherStatus {
        FileWatcherStatus {
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
pub struct FileWatcherMonitor {
    state: Arc<WatcherState>,
}

impl FileWatcherMonitor {
    #[must_use]
    pub fn status(&self) -> FileWatcherStatus {
        self.state.snapshot()
    }
}

impl fmt::Debug for FileWatcherMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileWatcherMonitor")
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

pub struct FileWatcherSupervisor {
    gate: Arc<PublicationGate>,
    wake: WakeQueue,
    state: Arc<WatcherState>,
    worker: Option<JoinHandle<()>>,
}

impl FileWatcherSupervisor {
    pub(crate) fn check_if_configured(
        reconcilers: &[Arc<FileReconciler>],
        config: FileWatcherConfig,
    ) -> Result<(), FileWatcherError> {
        if reconcilers.is_empty() {
            return Ok(());
        }
        validate_configuration(reconcilers.len(), config)?;
        let (wake, _receiver, _state) = WakeQueue::new();
        let _watcher = WatcherBackend::new(reconcilers, &wake)?;
        Ok(())
    }

    /// Starts the direct-file watcher when at least one reconciler is configured.
    ///
    /// # Errors
    ///
    /// Returns an error when watcher configuration, directory setup, backend installation, or
    /// worker startup fails.
    pub fn start_if_configured(
        reconcilers: Vec<Arc<FileReconciler>>,
        config: FileWatcherConfig,
    ) -> Result<Option<Self>, FileWatcherError> {
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
        reconcilers: Vec<Arc<FileReconciler>>,
        config: FileWatcherConfig,
    ) -> Result<Self, FileWatcherError> {
        validate_configuration(reconcilers.len(), config)?;
        let (wake, receiver, state) = WakeQueue::new();
        let watcher = WatcherBackend::new(&reconcilers, &wake)?;
        let gate = Arc::new(PublicationGate::new());
        let worker_gate = Arc::clone(&gate);
        let worker_state = Arc::clone(&state);
        let worker_wake = wake.clone();
        state.running.store(true, Ordering::Release);
        let worker = thread::Builder::new()
            .name("direct-file-watcher".into())
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
                FileWatcherError::Thread { source }
            })?;
        wake.event();

        Ok(Self {
            gate,
            wake,
            state,
            worker: Some(worker),
        })
    }

    #[must_use]
    pub fn status(&self) -> FileWatcherStatus {
        self.state.snapshot()
    }

    #[must_use]
    pub fn monitor(&self) -> FileWatcherMonitor {
        FileWatcherMonitor {
            state: Arc::clone(&self.state),
        }
    }

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

impl fmt::Debug for FileWatcherSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileWatcherSupervisor")
            .field("status", &self.status())
            .field("running", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for FileWatcherSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct WatcherBackend {
    _watcher: RecommendedWatcher,
}

impl WatcherBackend {
    fn new(
        reconcilers: &[Arc<FileReconciler>],
        wake: &WakeQueue,
    ) -> Result<Self, FileWatcherError> {
        let callback_wake = wake.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            handle_notify_event(&callback_wake, event);
        })
        .map_err(|source| FileWatcherError::Create { source })?;
        for path in watch_paths(reconcilers)? {
            watcher
                .watch(&path, RecursiveMode::NonRecursive)
                .map_err(|source| FileWatcherError::Watch { path, source })?;
        }
        Ok(Self { _watcher: watcher })
    }

    fn rebuild(
        &mut self,
        reconcilers: &[Arc<FileReconciler>],
        wake: &WakeQueue,
    ) -> Result<(), FileWatcherError> {
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
    config: FileWatcherConfig,
) -> Result<(), FileWatcherError> {
    if reconciler_count == 0 {
        return Err(FileWatcherError::NoIdentities);
    }
    if reconciler_count > MAX_RECONCILERS {
        return Err(FileWatcherError::TooManyIdentities {
            count: reconciler_count,
        });
    }
    if config.rescan_interval < MIN_RESCAN_INTERVAL {
        return Err(FileWatcherError::RescanIntervalTooShort);
    }
    if config.event_debounce.is_zero() || config.event_debounce > config.event_max_delay {
        return Err(FileWatcherError::InvalidEventDebounce);
    }
    if config.event_max_delay > config.rescan_interval {
        return Err(FileWatcherError::InvalidEventMaxDelay);
    }
    Ok(())
}

fn watch_paths(reconcilers: &[Arc<FileReconciler>]) -> Result<BTreeSet<PathBuf>, FileWatcherError> {
    let mut paths = BTreeSet::new();
    for reconciler in reconcilers {
        for configured in [
            reconciler.certificate_chain_path(),
            reconciler.private_key_path(),
        ] {
            let parent = configured
                .parent()
                .ok_or_else(|| FileWatcherError::Directory {
                    path: configured.into(),
                    source: io::Error::new(io::ErrorKind::InvalidInput, "file has no parent"),
                })?;
            paths.insert(canonical_watch_path(parent)?);
            if let Some(grandparent) = parent.parent() {
                paths.insert(canonical_watch_path(grandparent)?);
            }
        }
    }
    Ok(paths)
}

fn canonical_watch_path(path: &Path) -> Result<PathBuf, FileWatcherError> {
    let canonical = std::fs::canonicalize(path).map_err(|source| FileWatcherError::Directory {
        path: path.into(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(FileWatcherError::PathNotDirectory { path: canonical });
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
    reconcilers: &[Arc<FileReconciler>],
    config: FileWatcherConfig,
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
                    log::warn!("direct-file reconciliation failed: {error}");
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
                log::error!("failed to refresh direct-file filesystem watches: {error}");
                state.mark_backend_degraded();
            }
        }
    }
}

fn debounce_events(
    receiver: &Receiver<()>,
    gate: &PublicationGate,
    config: FileWatcherConfig,
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
