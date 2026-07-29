use std::{
    path::Path,
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
    GenerationManager,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
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
    /// Watches the canonical file's parent so atomic rename replacement is observed, and performs
    /// periodic hash reconciliation in case the backend drops an event.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be watched.
    pub fn start(
        coordinator: CanonicalConfigCoordinator,
        generations: GenerationManager,
        options: ConfigWatcherOptions,
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
                if event.is_ok() {
                    let _ = watcher_events.try_send(());
                }
            },
            notify::Config::default(),
        )?;
        watcher.watch(&parent, RecursiveMode::NonRecursive)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(WatcherCounters::default());
        counters.running.store(true, Ordering::Release);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_counters = Arc::clone(&counters);
        let thread = thread::Builder::new()
            .name("oxiroute-config-watch".into())
            .spawn(move || {
                let _watcher = watcher;
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
                    reconcile(&coordinator, &generations, &thread_counters);
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

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
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

fn reconcile(
    coordinator: &CanonicalConfigCoordinator,
    generations: &GenerationManager,
    counters: &WatcherCounters,
) {
    counters.reconciliations.fetch_add(1, Ordering::Relaxed);
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => {
            let status = generations.status();
            generations.observe_disk_revision(document.disk_revision.clone());
            let revision = &document.candidate_revision;
            if status.active_revision.as_ref() != Some(revision)
                && status.candidate_revision.as_ref() != Some(revision)
                && status.quarantined_revision.as_ref() != Some(revision)
                && generations.prepare(*document).is_err()
            {
                counters.rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
        ConfigLoadOutcome::Rejected(_) => {
            counters.rejected.fetch_add(1, Ordering::Relaxed);
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
    use std::{fs, time::Instant};

    use oxiroute_config::{Config, render_lua};
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
    fn parent_watch_reconciles_atomic_replacement_and_rejects_invalid_content() {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        let mut config = empty_config();
        fs::write(&path, render_lua(&config).expect("initial render")).expect("initial config");
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
        )
        .expect("watcher");

        config.max_connections = Some(7);
        let replacement = directory.path().join("candidate.lua");
        fs::write(&replacement, render_lua(&config).expect("candidate render")).expect("candidate");
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
        let initial_candidate_revision = initial.candidate_revision.clone();
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

    fn haproxy_source(listener_port: u16, upstream_port: u16) -> String {
        format!(
            "defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout queue 15s\n  timeout client 5m\n  timeout server 5m\nfrontend database\n  bind 127.0.0.1:{listener_port}\n  default_backend database_pool\nbackend database_pool\n  balance roundrobin\n  server primary 127.0.0.1:{upstream_port}\n"
        )
    }

    fn empty_config() -> Config {
        Config {
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
