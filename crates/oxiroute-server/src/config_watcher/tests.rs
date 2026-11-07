use std::{fs, sync::mpsc::TryRecvError, time::Instant};

use notify::{
    Event, EventKind,
    event::{
        AccessKind, AccessMode, CreateKind, DataChange, Flag, ModifyKind, RemoveKind, RenameMode,
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
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
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
    let mut watcher =
        ConfigWatcher::start(coordinator, manager, options, RuntimeMode::Direct).expect("watcher");

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
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
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
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
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
        format!("server {{ listen 127.0.0.1:{first_port}; location / {{ return 200 first; }} }}"),
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

fn write_lua(path: &Path, config: &ConfigDraft) {
    let config = config.clone().validate().expect("valid config");
    fs::write(
        path,
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
            .expect("render"),
    )
    .expect("config");
}

fn activated(path: &Path) -> (CanonicalConfigCoordinator, GenerationManager) {
    let coordinator = CanonicalConfigCoordinator::new(path).expect("coordinator");
    let manager = GenerationManager::new();
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("active load")
    };
    let candidate = manager.prepare(*document).expect("active prepare");
    manager.activate(&candidate).expect("active generation");
    (coordinator, manager)
}

#[test]
fn a_rejected_file_is_recorded_once_and_cleared_by_the_next_accepted_file() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("oxiroute.lua");
    write_lua(&path, &empty_config());
    let (coordinator, manager) = activated(&path);
    let counters = WatcherCounters::default();
    let reconcile = || reconcile(&coordinator, &manager, &counters, RuntimeMode::Direct);

    fs::write(&path, "return {").expect("invalid edit");
    assert!(reconcile().is_none());
    assert!(reconcile().is_none());

    let status = manager.status();
    let rejection = status.last_rejection.expect("recorded rejection");
    assert_eq!(rejection.diagnostics[0].code, "E_SYNTAX");
    assert!(rejection.revision.is_some());
    assert!(status.degraded);
    assert_eq!(status.config_rejections, 1);
    assert_eq!(counters.rejected.load(Ordering::Relaxed), 2);

    write_lua(&path, &empty_config());
    assert!(reconcile().is_some());

    let status = manager.status();
    assert!(status.last_rejection.is_none());
    assert!(!status.degraded);
    assert_eq!(status.config_rejections, 1);
}

#[test]
fn a_file_that_cannot_be_prepared_is_recorded_with_the_preparation_code() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied port");
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("oxiroute.lua");
    write_lua(&path, &empty_config());
    let (coordinator, manager) = activated(&path);
    let counters = WatcherCounters::default();

    let mut config = empty_config();
    config.stats = Some(oxiroute_config::Stats {
        binds: vec![occupied.local_addr().expect("occupied address")],
        admin_token_file: None,
        pages: Vec::new(),
    });
    write_lua(&path, &config);
    assert!(reconcile(&coordinator, &manager, &counters, RuntimeMode::Direct).is_some());
    assert!(reconcile(&coordinator, &manager, &counters, RuntimeMode::Direct).is_some());

    let status = manager.status();
    let rejection = status.last_rejection.expect("recorded rejection");
    assert_eq!(rejection.diagnostics[0].code, "listener_reservation");
    assert_eq!(status.config_rejections, 1);
    assert!(status.quarantined_revision.is_some());

    write_lua(&path, &empty_config());
    assert!(reconcile(&coordinator, &manager, &counters, RuntimeMode::Direct).is_some());
    assert!(manager.status().last_rejection.is_none());
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
        registry: None,
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
