use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::{
    GenerationError, GenerationManager, RuntimeMode,
    config::coordinator::{
        CanonicalConfigCoordinator, ConfigDiagnostic, ConfigDiagnosticSeverity,
        ConfigDiagnosticStage, ConfigLoadOutcome, ConfigRejection,
    },
    config::events::ConfigEventFilter,
    generation::GENERATION_PREPARATION_TIMEOUT,
};

#[derive(Clone, Copy, Debug)]
pub struct ConfigWatcherOptions {
    pub debounce: Duration,
    pub max_debounce: Duration,
    pub reconciliation_interval: Duration,
}

impl Default for ConfigWatcherOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(250),
            max_debounce: Duration::from_secs(2),
            reconciliation_interval: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigWatcherStatus {
    pub events: u64,
    pub reconciliations: u64,
    pub rejected: u64,
    pub running: bool,
}

#[derive(Default)]
struct WatcherCounters {
    events: AtomicU64,
    reconciliations: AtomicU64,
    rejected: AtomicU64,
    running: AtomicBool,
}

pub struct ConfigWatcher {
    counters: Arc<WatcherCounters>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    wake: mpsc::SyncSender<()>,
}

impl ConfigWatcher {
    /// Watches the canonical file's parent and resolved native inputs so atomic replacement and
    /// include/glob changes are observed, and performs periodic reconciliation in case the backend
    /// drops an event.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be watched.
    pub fn start(
        coordinator: CanonicalConfigCoordinator,
        generations: GenerationManager,
        options: ConfigWatcherOptions,
        mode: RuntimeMode,
    ) -> notify::Result<Self> {
        validate_options(options)?;
        let parent = coordinator
            .canonical_path()
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        let watcher_events = events_tx.clone();
        let initial_dependencies = match coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document.dependencies.clone(),
            ConfigLoadOutcome::Rejected(_) => Vec::new(),
        };
        let filter = ConfigEventFilter::new(coordinator.canonical_path(), &initial_dependencies);
        let watcher_filter = filter.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                handle_notify_event(&watcher_events, &watcher_filter, event);
            },
            notify::Config::default(),
        )?;
        let mut watched_paths = HashSet::new();
        rebuild_watches(
            &mut watcher,
            &parent,
            &initial_dependencies,
            &mut watched_paths,
        )?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(WatcherCounters::default());
        counters.running.store(true, Ordering::Release);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_counters = Arc::clone(&counters);
        let thread = thread::Builder::new()
            .name("oxiroute-config-watch".into())
            .spawn(move || {
                let mut watcher = watcher;
                let mut watched_paths = watched_paths;
                if let Some(dependencies) =
                    reconcile(&coordinator, &generations, &thread_counters, mode)
                {
                    filter.set_dependencies(&dependencies);
                    if rebuild_watches(&mut watcher, &parent, &dependencies, &mut watched_paths)
                        .is_err()
                    {
                        thread_counters.rejected.fetch_add(1, Ordering::Relaxed);
                    }
                }
                while !thread_shutdown.load(Ordering::Acquire) {
                    let event = events_rx
                        .recv_timeout(options.reconciliation_interval)
                        .is_ok();
                    if thread_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    if event {
                        thread_counters.events.fetch_add(1, Ordering::Relaxed);
                        let first = Instant::now();
                        loop {
                            let remaining = options
                                .max_debounce
                                .saturating_sub(first.elapsed())
                                .min(options.debounce);
                            if remaining.is_zero() || events_rx.recv_timeout(remaining).is_err() {
                                break;
                            }
                            thread_counters.events.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if let Some(dependencies) =
                        reconcile(&coordinator, &generations, &thread_counters, mode)
                    {
                        filter.set_dependencies(&dependencies);
                        if rebuild_watches(&mut watcher, &parent, &dependencies, &mut watched_paths)
                            .is_err()
                        {
                            thread_counters.rejected.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                thread_counters.running.store(false, Ordering::Release);
            })
            .map_err(|error| notify::Error::generic(&error.to_string()))?;
        Ok(Self {
            counters,
            shutdown,
            thread: Some(thread),
            wake: events_tx,
        })
    }

    #[must_use]
    pub fn status(&self) -> ConfigWatcherStatus {
        ConfigWatcherStatus {
            events: self.counters.events.load(Ordering::Relaxed),
            reconciliations: self.counters.reconciliations.load(Ordering::Relaxed),
            rejected: self.counters.rejected.load(Ordering::Relaxed),
            running: self.counters.running.load(Ordering::Acquire),
        }
    }

    pub fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle_notify_event(
    wake: &mpsc::SyncSender<()>,
    filter: &ConfigEventFilter,
    event: notify::Result<notify::Event>,
) {
    match event {
        Ok(event) if event.need_rescan() => {
            let _ = wake.try_send(());
        }
        Ok(event) if event.kind.is_access() || !filter.matches(&event) => {}
        Ok(_) | Err(_) => {
            let _ = wake.try_send(());
        }
    }
}

fn rebuild_watches(
    watcher: &mut RecommendedWatcher,
    root_parent: &Path,
    dependencies: &[PathBuf],
    watched_paths: &mut HashSet<PathBuf>,
) -> notify::Result<()> {
    let mut desired = HashSet::from([root_parent.to_path_buf()]);
    for dependency in dependencies {
        if dependency.exists() {
            desired.insert(dependency.clone());
        }
        if let Some(parent) = dependency
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            desired.insert(existing_parent(parent));
        }
    }

    for path in watched_paths.difference(&desired) {
        let _ = watcher.unwatch(path);
    }
    for path in desired.difference(watched_paths) {
        watcher.watch(path, RecursiveMode::NonRecursive)?;
    }
    *watched_paths = desired;
    Ok(())
}

fn existing_parent(path: &Path) -> PathBuf {
    let mut current = path;
    while !current.exists() {
        let Some(parent) = current.parent() else {
            return PathBuf::from(".");
        };
        if parent == current {
            return current.to_path_buf();
        }
        current = parent;
    }
    current.to_path_buf()
}

fn reconcile(
    coordinator: &CanonicalConfigCoordinator,
    generations: &GenerationManager,
    counters: &WatcherCounters,
    mode: RuntimeMode,
) -> Option<Vec<PathBuf>> {
    reconcile_with_preparation_timeout(
        coordinator,
        generations,
        counters,
        mode,
        GENERATION_PREPARATION_TIMEOUT,
    )
}

fn reconcile_with_preparation_timeout(
    coordinator: &CanonicalConfigCoordinator,
    generations: &GenerationManager,
    counters: &WatcherCounters,
    mode: RuntimeMode,
    preparation_timeout: Duration,
) -> Option<Vec<PathBuf>> {
    counters.reconciliations.fetch_add(1, Ordering::Relaxed);
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => {
            let dependencies = document.dependencies.clone();
            let status = generations.status();
            let disk_revision = document.authored_revision.clone();
            generations.observe_disk_revision(disk_revision.clone());
            let revision = &document.effective_revision;
            // A supervised process only adopts sockets: the master applies a listener-set change,
            // so this watcher leaves such a candidate alone there. Every process refuses a change
            // that no reload can apply.
            let restart_required = generations.active().is_some_and(|active| {
                active.listener_restart_required(&document.validated_config)
                    || (mode == RuntimeMode::Supervised
                        && active.listener_set_changed(&document.validated_config))
            });
            let quarantined = status.quarantined_revision.as_ref() == Some(revision);
            let preparation = if status.active_revision.as_ref() != Some(revision)
                && status.candidate_revision.as_ref() != Some(revision)
                && !quarantined
                && !restart_required
            {
                Some(
                    generations
                        .prepare_with_deadline(*document, Instant::now() + preparation_timeout),
                )
            } else {
                None
            };
            match preparation {
                Some(Err(GenerationError::PreparationTimedOut)) => {}
                Some(Err(error)) => {
                    counters.rejected.fetch_add(1, Ordering::Relaxed);
                    reject(
                        generations,
                        &ConfigRejection::new(
                            Some(&disk_revision),
                            &[preparation_diagnostic(&error)],
                        ),
                    );
                }
                _ if !quarantined => generations.accept_configuration(),
                _ => {}
            }
            Some(dependencies)
        }
        ConfigLoadOutcome::Rejected(rejection) => {
            counters.rejected.fetch_add(1, Ordering::Relaxed);
            reject(
                generations,
                &ConfigRejection::new(rejection.disk_revision.as_ref(), &rejection.diagnostics),
            );
            None
        }
    }
}

fn reject(generations: &GenerationManager, rejection: &ConfigRejection) {
    if generations.reject_configuration(rejection.clone()) {
        rejection.log();
    }
}

fn preparation_diagnostic(error: &GenerationError) -> ConfigDiagnostic {
    ConfigDiagnostic {
        code: error.code(),
        severity: ConfigDiagnosticSeverity::Error,
        stage: ConfigDiagnosticStage::Validation,
        message: "the configuration cannot be prepared as a complete runtime generation",
        path: None,
        detail: None,
        line: None,
    }
}

fn validate_options(options: ConfigWatcherOptions) -> notify::Result<()> {
    if options.debounce.is_zero()
        || options.max_debounce < options.debounce
        || options.reconciliation_interval.is_zero()
    {
        return Err(notify::Error::generic("invalid config watcher timing"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
