use std::{
    collections::BTreeSet,
    fmt, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{
    FileReconciler,
    certificate::PublicationGate,
    watcher_engine::{
        ReconcileResult, WakeQueue, WatcherEngine, WatcherMonitor, WatcherSource,
        WatcherStartError, WatcherStatus, handle_notify_event,
    },
};

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

impl From<WatcherStatus> for FileWatcherStatus {
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
    #[error("direct-file watcher event max delay must be no greater than periodic rescan interval")]
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

#[derive(Clone)]
pub struct FileWatcherMonitor {
    monitor: WatcherMonitor,
}

impl FileWatcherMonitor {
    #[must_use]
    pub fn status(&self) -> FileWatcherStatus {
        self.monitor.status().into()
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

pub struct FileWatcherSupervisor {
    engine: WatcherEngine,
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
        let (wake, _receiver, _monitor) = WakeQueue::new();
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
        let engine = WatcherEngine::start("direct-file-watcher", config.into(), move |wake| {
            let watcher = WatcherBackend::new(&reconcilers, wake)?;
            Ok(FileWatcherSource {
                reconcilers,
                watcher,
            })
        })
        .map_err(|error| match error {
            WatcherStartError::Source(error) => error,
            WatcherStartError::Thread(source) => FileWatcherError::Thread { source },
        })?;
        Ok(Self { engine })
    }

    #[must_use]
    pub fn status(&self) -> FileWatcherStatus {
        self.engine.status().into()
    }

    #[must_use]
    pub fn monitor(&self) -> FileWatcherMonitor {
        FileWatcherMonitor {
            monitor: self.engine.monitor(),
        }
    }

    pub fn shutdown(&mut self) {
        self.engine.shutdown();
    }
}

impl fmt::Debug for FileWatcherSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileWatcherSupervisor")
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

struct FileWatcherSource {
    reconcilers: Vec<Arc<FileReconciler>>,
    watcher: WatcherBackend,
}

impl WatcherSource for FileWatcherSource {
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
                    log::warn!("direct-file reconciliation failed: {error}");
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
                log::error!("failed to refresh direct-file filesystem watches: {error}");
                false
            }
        }
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
