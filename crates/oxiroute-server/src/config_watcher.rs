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
    GenerationManager, RuntimeMode,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
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
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                handle_notify_event(&watcher_events, event);
            },
            notify::Config::default(),
        )?;
        let initial_dependencies = match coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document.dependencies.clone(),
            ConfigLoadOutcome::Rejected(_) => Vec::new(),
        };
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
                    && rebuild_watches(&mut watcher, &parent, &dependencies, &mut watched_paths)
                        .is_err()
                {
                    thread_counters.rejected.fetch_add(1, Ordering::Relaxed);
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
                        && rebuild_watches(&mut watcher, &parent, &dependencies, &mut watched_paths)
                            .is_err()
                    {
                        thread_counters.rejected.fetch_add(1, Ordering::Relaxed);
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

fn handle_notify_event(wake: &mpsc::SyncSender<()>, event: notify::Result<notify::Event>) {
    match event {
        Ok(event) if event.need_rescan() => {
            let _ = wake.try_send(());
        }
        Ok(event) if event.kind.is_access() => {}
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
            generations.observe_disk_revision(document.authored_revision.clone());
            let revision = &document.effective_revision;
            let restart_required = generations.active().is_some_and(|active| {
                active.listener_restart_required(mode, &document.validated_config)
            });
            let preparation = if status.active_revision.as_ref() != Some(revision)
                && status.candidate_revision.as_ref() != Some(revision)
                && status.quarantined_revision.as_ref() != Some(revision)
                && !restart_required
            {
                Some(
                    generations
                        .prepare_with_deadline(*document, Instant::now() + preparation_timeout),
                )
            } else {
                None
            };
            if preparation.is_some_and(|result| {
                result.is_err_and(|error| {
                    !matches!(error, crate::GenerationError::PreparationTimedOut)
                })
            }) {
                counters.rejected.fetch_add(1, Ordering::Relaxed);
            }
            Some(dependencies)
        }
        ConfigLoadOutcome::Rejected(_) => {
            counters.rejected.fetch_add(1, Ordering::Relaxed);
            None
        }
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
mod tests {
    use std::{fs, sync::mpsc::TryRecvError, time::Instant};

    use notify::{
        Event, EventKind,
        event::{
            AccessKind, AccessMode, CreateKind, DataChange, Flag, ModifyKind, RemoveKind,
            RenameMode,
        },
    };
    use oxiroute_config::ConfigDraft;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn rejects_unbounded_or_inverted_timing() {
        let invalid = ConfigWatcherOptions {
            debounce: Duration::from_secs(2),
            max_debounce: Duration::from_secs(1),
            reconciliation_interval: Duration::from_secs(30),
        };
        assert!(validate_options(invalid).is_err());
    }

    #[test]
    fn notify_access_events_are_ignored() {
        let (wake, receiver) = mpsc::sync_channel(1);
        for kind in [
            AccessKind::Read,
            AccessKind::Open(AccessMode::Read),
            AccessKind::Close(AccessMode::Read),
        ] {
            handle_notify_event(&wake, Ok(Event::new(EventKind::Access(kind))));
        }

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn notify_rescans_changes_and_backend_errors_wake_reconciliation() {
        let (wake, receiver) = mpsc::sync_channel(1);
        let events = [
            Ok(Event::new(EventKind::Access(AccessKind::Read)).set_flag(Flag::Rescan)),
            Ok(Event::new(EventKind::Modify(ModifyKind::Data(
                DataChange::Any,
            )))),
            Ok(Event::new(EventKind::Create(CreateKind::File))),
            Ok(Event::new(EventKind::Remove(RemoveKind::File))),
            Ok(Event::new(EventKind::Modify(ModifyKind::Name(
                RenameMode::Any,
            )))),
            Err(notify::Error::generic("backend error")),
        ];

        for event in events {
            handle_notify_event(&wake, event);
            receiver.try_recv().expect("reconciliation wake");
        }
    }

    #[test]
    fn periodic_reconciliation_does_not_feed_access_events_back() {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        let config = empty_config().validate().expect("valid config");
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Lua,
                &config,
            )
            .expect("render"),
        )
        .expect("config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let manager = GenerationManager::new();
        let options = ConfigWatcherOptions {
            debounce: Duration::from_millis(10),
            max_debounce: Duration::from_millis(30),
            reconciliation_interval: Duration::from_millis(80),
        };
        let started = Instant::now();
        let mut watcher = ConfigWatcher::start(coordinator, manager, options, RuntimeMode::Direct)
            .expect("watcher");

        wait_until(|| watcher.status().reconciliations >= 4);

        let status = watcher.status();
        assert_eq!(status.events, 0);
        assert!(started.elapsed() >= options.reconciliation_interval * 3);
        watcher.shutdown();
    }

    #[test]
    fn parent_watch_reconciles_atomic_replacement_and_rejects_invalid_content() {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        let mut config = empty_config();
        let initial_config = config.clone().validate().expect("valid initial config");
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Lua,
                &initial_config,
            )
            .expect("initial render"),
        )
        .expect("initial config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let manager = GenerationManager::new();
        let ConfigLoadOutcome::Loaded(initial) = coordinator.load() else {
            panic!("initial load")
        };
        let candidate = manager.prepare(*initial).expect("initial prepare");
        let initial = manager.activate(&candidate).expect("initial activation");
        let initial_revision = initial.revision().candidate.clone();
        let mut watcher = ConfigWatcher::start(
            coordinator,
            manager.clone(),
            ConfigWatcherOptions {
                debounce: Duration::from_millis(10),
                max_debounce: Duration::from_millis(30),
                reconciliation_interval: Duration::from_millis(40),
            },
            RuntimeMode::Direct,
        )
        .expect("watcher");

        config.max_connections = Some(7);
        let replacement = directory.path().join("candidate.lua");
        let config = config.validate().expect("valid candidate config");
        fs::write(
            &replacement,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Lua,
                &config,
            )
            .expect("candidate render"),
        )
        .expect("candidate");
        fs::rename(&replacement, &path).expect("atomic replacement");
        wait_until(|| {
            manager
                .status()
                .candidate_revision
                .as_ref()
                .is_some_and(|revision| revision != &initial_revision)
        });
        let candidate = manager.candidate().expect("queued candidate");
        manager
            .activate(&candidate)
            .expect("activate queued candidate");
        let active_revision = manager.status().active_revision;

        fs::write(&path, "return {").expect("invalid external edit");
        wait_until(|| watcher.status().rejected > 0);
        assert_eq!(manager.status().active_revision, active_revision);

        let started = Instant::now();
        watcher.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn reconciliation_projects_structural_candidates_by_explicit_runtime_mode() {
        for (mode, pending) in [
            (RuntimeMode::Direct, true),
            (RuntimeMode::Supervised, false),
        ] {
            let directory = TempDir::new().expect("directory");
            let path = directory.path().join("oxiroute.lua");
            let active = empty_config().validate().expect("valid active config");
            fs::write(
                &path,
                oxiroute_config_source::render_config(
                    oxiroute_config_source::ConfigFormat::Lua,
                    &active,
                )
                .unwrap(),
            )
            .unwrap();
            let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
            let manager = match mode {
                RuntimeMode::Direct => GenerationManager::new(),
                RuntimeMode::Supervised => GenerationManager::new_supervised(),
            };
            let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
                panic!("active load")
            };
            let candidate = manager.prepare(*document).expect("active prepare");
            manager.activate(&candidate).expect("active generation");

            let mut changed = empty_config();
            changed.stats = Some(oxiroute_config::Stats {
                binds: vec!["127.0.0.1:18404".parse().unwrap()],
                admin_token_file: None,
                pages: Vec::new(),
            });
            let changed = changed.validate().expect("valid changed config");
            let changed_revision = oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Lua,
                &changed,
            )
            .unwrap();
            fs::write(&path, changed_revision).unwrap();
            let counters = WatcherCounters::default();

            assert!(reconcile(&coordinator, &manager, &counters, mode).is_some());
            assert_eq!(manager.status().candidate_revision.is_some(), pending);
            assert!(manager.status().quarantined_revision.is_none());
            assert_eq!(counters.rejected.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn expired_preparation_is_retryable_and_does_not_count_as_rejected() {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        let config = empty_config().validate().expect("valid config");
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Lua,
                &config,
            )
            .expect("render"),
        )
        .expect("config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let manager = GenerationManager::new();
        let counters = WatcherCounters::default();
        let result = reconcile_with_preparation_timeout(
            &coordinator,
            &manager,
            &counters,
            RuntimeMode::Direct,
            Duration::ZERO,
        );

        assert!(result.is_some());
        assert_eq!(counters.rejected.load(Ordering::Acquire), 0);
    }

    #[test]
    fn periodic_reconciliation_prepares_native_dependency_changes() {
        let directory = TempDir::new().expect("directory");
        let native_directory = directory.path().join("native");
        fs::create_dir(&native_directory).expect("native directory");
        let native_path = native_directory.join("haproxy.cfg");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        fs::write(&native_path, haproxy_source(port, 5432)).expect("native source");
        let path = directory.path().join("oxiroute.kdl");
        fs::write(&path, "haproxy_server \"native/haproxy.cfg\"\n").expect("root source");
        let root_bytes = fs::read(&path).unwrap();
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let manager = GenerationManager::new();
        let ConfigLoadOutcome::Loaded(initial) = coordinator.load() else {
            panic!("initial load")
        };
        let initial_candidate_revision = initial.effective_revision.clone();
        let initial = manager.prepare(*initial).expect("initial prepare");
        manager.activate(&initial).expect("initial activation");
        let mut watcher = ConfigWatcher::start(
            coordinator,
            manager.clone(),
            ConfigWatcherOptions {
                debounce: Duration::from_millis(10),
                max_debounce: Duration::from_millis(30),
                reconciliation_interval: Duration::from_millis(40),
            },
            RuntimeMode::Direct,
        )
        .expect("watcher");

        fs::write(&native_path, haproxy_source(port, 5433)).expect("native edit");
        wait_until(|| {
            manager
                .status()
                .candidate_revision
                .as_ref()
                .is_some_and(|revision| revision != &initial_candidate_revision)
        });

        assert_eq!(fs::read(&path).unwrap(), root_bytes);
        assert!(watcher.status().reconciliations > 0);
        watcher.shutdown();
    }

    #[test]
    fn dependency_watch_reconciles_a_deleted_nginx_glob_without_waiting_for_periodic_interval() {
        let directory = TempDir::new().expect("directory");
        let native_directory = directory.path().join("native");
        let sites_directory = native_directory.join("sites-enabled");
        fs::create_dir_all(&sites_directory).expect("native directories");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        fs::write(
            native_directory.join("nginx.conf"),
            b"events {} http { access_log off; include sites-enabled/*.conf; }",
        )
        .expect("nginx root");
        let site = sites_directory.join("site.conf");
        fs::write(
            &site,
            format!("server {{ listen 127.0.0.1:{port}; location / {{ return 200 ok; }} }}"),
        )
        .expect("nginx site");
        let path = directory.path().join("oxiroute.kdl");
        fs::write(
            &path,
            "version 1\nnginx_server \"native/nginx.conf\" { root_prefix \"native\" }\n",
        )
        .expect("root source");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let manager = GenerationManager::new();
        let ConfigLoadOutcome::Loaded(initial) = coordinator.load() else {
            panic!("initial load")
        };
        let initial_candidate_revision = initial.effective_revision.clone();
        let initial = manager.prepare(*initial).expect("initial prepare");
        manager.activate(&initial).expect("initial activation");
        let mut watcher = ConfigWatcher::start(
            coordinator,
            manager.clone(),
            ConfigWatcherOptions {
                debounce: Duration::from_millis(10),
                max_debounce: Duration::from_millis(30),
                reconciliation_interval: Duration::from_secs(30),
            },
            RuntimeMode::Direct,
        )
        .expect("watcher");

        fs::remove_file(site).expect("delete nginx site");
        wait_until(|| {
            manager
                .status()
                .candidate_revision
                .as_ref()
                .is_some_and(|revision| revision != &initial_candidate_revision)
        });

        let candidate = manager.candidate().expect("queued candidate");
        assert!(
            candidate
                .generation()
                .config()
                .as_draft()
                .listeners
                .is_empty()
        );
        assert!(watcher.status().events > 0);
        watcher.shutdown();
    }

    #[test]
    fn dependency_watch_rebuilds_for_nginx_glob_add_and_rename() {
        let directory = TempDir::new().expect("directory");
        let native_directory = directory.path().join("native");
        let sites_directory = native_directory.join("sites-enabled");
        fs::create_dir_all(&sites_directory).expect("native directories");
        fs::write(
            native_directory.join("nginx.conf"),
            b"events {} http { access_log off; include sites-enabled/*.conf; }",
        )
        .expect("nginx root");
        let path = directory.path().join("oxiroute.kdl");
        fs::write(
            &path,
            "version 1\nnginx_server \"native/nginx.conf\" { root_prefix \"native\" }\n",
        )
        .expect("root source");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let manager = GenerationManager::new();
        let ConfigLoadOutcome::Loaded(initial) = coordinator.load() else {
            panic!("initial load")
        };
        let initial_revision = initial.effective_revision.clone();
        let initial = manager.prepare(*initial).expect("initial prepare");
        manager.activate(&initial).expect("initial activation");
        let mut watcher = ConfigWatcher::start(
            coordinator,
            manager.clone(),
            ConfigWatcherOptions {
                debounce: Duration::from_millis(10),
                max_debounce: Duration::from_millis(30),
                reconciliation_interval: Duration::from_secs(30),
            },
            RuntimeMode::Direct,
        )
        .expect("watcher");

        let first_port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("first listener")
            .local_addr()
            .unwrap()
            .port();
        let first = sites_directory.join("first.conf");
        fs::write(
            &first,
            format!(
                "server {{ listen 127.0.0.1:{first_port}; location / {{ return 200 first; }} }}"
            ),
        )
        .expect("add glob match");
        wait_until(|| {
            manager
                .status()
                .candidate_revision
                .as_ref()
                .is_some_and(|revision| revision != &initial_revision)
        });
        let added_revision = manager.status().candidate_revision.expect("added revision");

        let renamed = sites_directory.join("renamed.conf");
        fs::rename(&first, &renamed).expect("rename glob match");
        let renamed_port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("renamed listener")
            .local_addr()
            .unwrap()
            .port();
        fs::write(
            &renamed,
            format!(
                "server {{ listen 127.0.0.1:{renamed_port}; location / {{ return 200 renamed; }} }}"
            ),
        )
        .expect("change renamed match");
        wait_until(|| {
            manager
                .status()
                .candidate_revision
                .as_ref()
                .is_some_and(|revision| revision != &added_revision)
        });
        let renamed_revision = manager
            .status()
            .candidate_revision
            .expect("renamed revision");

        fs::remove_file(renamed).expect("remove glob match");
        assert_ne!(added_revision, renamed_revision);
        assert!(watcher.status().events > 0);
        watcher.shutdown();
    }

    fn haproxy_source(listener_port: u16, upstream_port: u16) -> String {
        format!(
            "defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout queue 15s\n  timeout client 5m\n  timeout server 5m\nfrontend database\n  bind 127.0.0.1:{listener_port}\n  default_backend database_pool\nbackend database_pool\n  balance roundrobin\n  server primary 127.0.0.1:{upstream_port}\n"
        )
    }

    fn empty_config() -> ConfigDraft {
        ConfigDraft {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "condition timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
