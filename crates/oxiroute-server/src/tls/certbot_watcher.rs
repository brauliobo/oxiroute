use std::{
    collections::BTreeSet,
    fmt, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

#[cfg(test)]
use super::watcher_engine::DebounceWindow;
use super::{
    CertbotReconciler,
    certificate::PublicationGate,
    watcher_engine::{
        ReconcileResult, WakeQueue, WatcherEngine, WatcherMonitor, WatcherSource,
        WatcherStartError, WatcherStatus, WatcherTiming, handle_notify_event,
    },
};

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

impl From<CertbotWatcherConfig> for WatcherTiming {
    fn from(config: CertbotWatcherConfig) -> Self {
        Self {
            rescan_interval: config.rescan_interval,
            event_debounce: config.event_debounce,
            event_max_delay: config.event_max_delay,
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

impl From<WatcherStatus> for CertbotWatcherStatus {
    fn from(status: WatcherStatus) -> Self {
        Self {
            running: status.running,
            degraded: status.degraded,
            coalesced_events: status.coalesced_events,
            ignored_access_events: status.ignored_access_events,
            backend_errors: status.backend_errors,
            watch_recoveries: status.watch_recoveries,
            watch_refreshes: status.watch_refreshes,
            rescans: status.rescans,
            periodic_rescans: status.periodic_rescans,
            reconciliation_failures: status.reconciliation_failures,
        }
    }
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

#[derive(Clone)]
pub struct CertbotWatcherMonitor {
    monitor: WatcherMonitor,
}

impl CertbotWatcherMonitor {
    #[must_use]
    pub fn status(&self) -> CertbotWatcherStatus {
        self.monitor.status().into()
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

pub struct CertbotWatcherSupervisor {
    engine: WatcherEngine,
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
        let (wake, _receiver, _monitor) = WakeQueue::new();
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

        let timing = config.into();
        let engine = WatcherEngine::start("certbot-watcher", timing, move |wake| {
            let watcher = WatcherBackend::new(&reconcilers, wake)?;
            Ok(CertbotWatcherSource {
                reconcilers,
                watcher,
            })
        })
        .map_err(|error| match error {
            WatcherStartError::Source(error) => error,
            WatcherStartError::Thread(source) => CertbotWatcherError::Thread { source },
        })?;
        Ok(Self { engine })
    }

    #[must_use]
    pub fn status(&self) -> CertbotWatcherStatus {
        self.engine.status().into()
    }

    #[must_use]
    pub fn monitor(&self) -> CertbotWatcherMonitor {
        CertbotWatcherMonitor {
            monitor: self.engine.monitor(),
        }
    }

    /// Stops publication, wakes the worker, and waits for its current local-filesystem work.
    ///
    /// Local filesystem operations are synchronous and are not unsafely cancelled. A blocked local
    /// filesystem can therefore delay this call after the main process has completed bounded
    /// service-runtime shutdown.
    pub fn shutdown(&mut self) {
        self.engine.shutdown();
    }
}

impl fmt::Debug for CertbotWatcherSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertbotWatcherSupervisor")
            .field("status", &self.status())
            .field("running", &self.engine.is_running())
            .finish_non_exhaustive()
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

struct CertbotWatcherSource {
    reconcilers: Vec<Arc<CertbotReconciler>>,
    watcher: WatcherBackend,
}

impl WatcherSource for CertbotWatcherSource {
    fn reconcile(&mut self, gate: &PublicationGate) -> ReconcileResult {
        let mut failures = 0;
        for reconciler in &self.reconcilers {
            if gate.is_stopped() {
                return ReconcileResult::Stopped;
            }
            match reconciler.reconcile_while_running(gate) {
                Ok(Some(_outcome)) => {}
                Ok(None) => return ReconcileResult::Stopped,
                Err(error) => {
                    log::warn!("Certbot reconciliation failed: {error}");
                    failures += 1;
                }
            }
        }
        ReconcileResult::Completed { failures }
    }

    fn refresh(&mut self, wake: &WakeQueue) -> bool {
        match self.watcher.rebuild(&self.reconcilers, wake) {
            Ok(()) => true,
            Err(error) => {
                log::error!("failed to refresh Certbot filesystem watches: {error}");
                false
            }
        }
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

#[cfg(test)]
mod tests {
    use std::{sync::mpsc::TryRecvError, time::Instant};

    use notify::{
        Event, EventKind,
        event::{AccessKind, AccessMode, Flag},
    };

    use super::*;

    #[test]
    fn bounded_wake_queue_coalesces_events() {
        let (wake, receiver, monitor) = WakeQueue::new();

        wake.event();
        wake.event();
        wake.event();

        receiver.try_recv().unwrap();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(monitor.status().coalesced_events, 2);
        assert!(!monitor.status().degraded);
    }

    #[test]
    fn notify_access_open_and_read_events_are_ignored() {
        let (wake, receiver, monitor) = WakeQueue::new();
        for kind in [
            AccessKind::Read,
            AccessKind::Open(AccessMode::Read),
            AccessKind::Close(AccessMode::Read),
        ] {
            handle_notify_event(&wake, Ok(Event::new(EventKind::Access(kind))));
        }

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(monitor.status().ignored_access_events, 3);
    }

    #[test]
    fn backend_degradation_and_recovery_are_visible() {
        let (wake, receiver, monitor) = WakeQueue::new();

        wake.backend_error();
        receiver.try_recv().unwrap();
        assert!(monitor.status().degraded);

        wake.backend_recovered();
        let status = monitor.status();
        assert!(!status.degraded);
        assert_eq!(status.backend_errors, 1);
        assert_eq!(status.watch_recoveries, 1);
    }

    #[test]
    fn notify_overflow_rescan_flag_marks_degradation() {
        let (wake, receiver, monitor) = WakeQueue::new();
        let overflow = Event::new(EventKind::Other).set_flag(Flag::Rescan);

        handle_notify_event(&wake, Ok(overflow));

        receiver.try_recv().unwrap();
        assert!(monitor.status().degraded);
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
                    rescan_interval: MIN_RESCAN_INTERVAL
                        .checked_sub(Duration::from_millis(1))
                        .expect("minimum rescan interval exceeds one millisecond"),
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
