use std::{
    env, fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Barrier, mpsc},
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::{
    Config, HttpHostSelector, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction,
    HttpService, HttpVersionPolicy, Listener, ListenerBind, Protocol, RtmpApplication, RtmpService,
    UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, render_lua,
};
use oxiroute_config_source::{ConfigFormat, render_config};
use tempfile::TempDir;

use super::{storage::StorageFailure, *};

fn socket_bind(value: &str) -> ListenerBind {
    ListenerBind::Socket {
        address: value.parse().expect("valid test socket"),
    }
}

fn socket_endpoint(value: &str) -> UpstreamEndpoint {
    UpstreamEndpoint::Socket {
        address: value.parse().expect("valid test socket"),
    }
}

fn minimal_config() -> Config {
    Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        cache_stores: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

fn rtmp_config(name: &str) -> Config {
    let mut config = minimal_config();
    config.listeners.push(Listener {
        name: name.into(),
        bind: socket_bind("127.0.0.1:1935"),
        protocol: Protocol::Rtmp,
        service: Some("rtmp-service".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(10_000),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    config.rtmp_services.push(RtmpService {
        name: "rtmp-service".into(),
        outbound_chunk_size: 4_096,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "live".into(),
            live: true,
            idle_streams: true,
            publish: oxiroute_config::RtmpAccessPolicy::default(),
            play: oxiroute_config::RtmpAccessPolicy::default(),
            limits: oxiroute_config::RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: Vec::new(),
        }],
    });
    config
}

fn normalizable_config() -> Config {
    let mut config = minimal_config();
    config.listeners.push(Listener {
        name: "web-listener".into(),
        bind: socket_bind("127.0.0.1:8080"),
        protocol: Protocol::Http,
        service: Some("web-service".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(10_000),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    config.upstream_pools.push(UpstreamPool {
        name: "web-pool".into(),
        servers: Vec::new(),
        endpoints: vec![socket_endpoint("127.0.0.1:3000")],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
    });
    config.http_services.push(HttpService {
        name: "web-service".into(),
        routes: vec![HttpRoute {
            host: Some(HttpHostSelector::NormalizedHost {
                value: "API.EXAMPLE.TEST".into(),
            }),
            path: HttpPathSelector::SegmentPrefix {
                value: "/v1/".into(),
            },
            methods: vec!["GET".into()],
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::Proxy {
                upstream_pool: "web-pool".into(),
                policy: HttpProxyPolicy::default(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 30_000,
        max_request_body_bytes: Some(10 * 1024 * 1024),
        gzip: None,
        access_log: None,
    });
    config
}

fn fixture(config: &Config) -> (TempDir, std::path::PathBuf, CanonicalConfigCoordinator) {
    let temp = TempDir::new().expect("temporary directory");
    let path = temp.path().join("oxiroute.lua");
    fs::write(&path, render_lua(config).expect("test config renders")).expect("write config");
    let coordinator = CanonicalConfigCoordinator::new(path.clone()).expect("coordinator");
    (temp, path, coordinator)
}

fn loaded(outcome: ConfigLoadOutcome) -> CanonicalConfigDocument {
    let ConfigLoadOutcome::Loaded(document) = outcome else {
        panic!("load rejected")
    };
    *document
}

fn no_temporary_entries(directory: &Path) -> bool {
    fs::read_dir(directory)
        .expect("read fixture directory")
        .all(|entry| {
            !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
}

fn lock_entry(directory: &Path) -> Option<PathBuf> {
    fs::read_dir(directory)
        .expect("read fixture directory")
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".oxiroute-config.") && name.ends_with(".lock")
        })
        .map(|entry| entry.path())
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for child process"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn save_child(
    mode: &str,
    path: &Path,
    expected: &ConfigRevision,
    result: &Path,
    ready: &Path,
    release: &Path,
) -> Child {
    Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("config_coordinator::tests::child_process_save_helper")
        .arg("--nocapture")
        .env("OXIROUTE_CONFIG_SAVE_CHILD", mode)
        .env("OXIROUTE_CONFIG_SAVE_PATH", path)
        .env("OXIROUTE_CONFIG_SAVE_EXPECTED", expected.as_str())
        .env("OXIROUTE_CONFIG_SAVE_RESULT", result)
        .env("OXIROUTE_CONFIG_SAVE_READY", ready)
        .env("OXIROUTE_CONFIG_SAVE_RELEASE", release)
        .spawn()
        .unwrap()
}

#[test]
fn load_hashes_exact_disk_bytes_and_returns_a_normalized_preview() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oxiroute.lua");
    let source = b"return { version = 1, listeners = {} }\n";
    fs::write(&path, source).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();

    let document = loaded(coordinator.load());

    assert_eq!(document.disk_revision, ConfigRevision::from_bytes(source));
    assert!(document.normalized_config.listeners.is_empty());
    assert!(document.normalized_config.rtmp_services.is_empty());
    assert_eq!(
        document.candidate_revision,
        ConfigRevision::from_bytes(
            render_config(ConfigFormat::Kdl, &document.normalized_config)
                .unwrap()
                .as_bytes()
        )
    );
    assert_ne!(document.disk_revision, document.candidate_revision);
    assert_eq!(
        oxiroute_config::load_lua(&document.config_preview).unwrap(),
        document.normalized_config
    );
    assert!(document.diagnostics.is_empty());
}

#[test]
fn typed_validation_is_normalized_and_deterministic() {
    let (_temp, _path, coordinator) = fixture(&minimal_config());

    let ConfigValidationOutcome::Valid(first) = coordinator.validate(&normalizable_config()) else {
        panic!("first draft is invalid")
    };
    let ConfigValidationOutcome::Valid(second) = coordinator.validate(&normalizable_config())
    else {
        panic!("second draft is invalid")
    };
    let first = *first;
    let second = *second;

    assert_eq!(first, second);
    assert_eq!(
        first.normalized_config.http_services[0].routes[0]
            .host
            .as_ref(),
        Some(&HttpHostSelector::NormalizedHost {
            value: "api.example.test".into(),
        })
    );
    assert_eq!(
        &first.normalized_config.http_services[0].routes[0].path,
        &HttpPathSelector::SegmentPrefix {
            value: "/v1/".into(),
        }
    );
    assert_eq!(
        render_lua(&first.normalized_config).unwrap(),
        first.config_preview
    );
}

#[test]
fn materialized_formats_load_and_save_in_their_authored_format() {
    let directory = TempDir::new().unwrap();
    for (extension, format) in [
        ("kdl", ConfigFormat::Kdl),
        ("lua", ConfigFormat::Lua),
        ("uci", ConfigFormat::Uci),
        ("hocon", ConfigFormat::Hocon),
    ] {
        let path = directory.path().join(format!("oxiroute.{extension}"));
        let source = render_config(format, &minimal_config()).unwrap();
        fs::write(&path, &source).unwrap();
        let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
        let document = loaded(coordinator.load());

        assert_eq!(document.format, format);
        assert!(!document.compositional);
        assert!(document.dependencies.is_empty());
        assert_eq!(document.config_preview, source);
        assert_eq!(
            document.disk_revision,
            ConfigRevision::from_bytes(source.as_bytes())
        );

        let mut draft = minimal_config();
        draft.max_connections = Some(17);
        let ConfigSaveOutcome::Saved(saved) = coordinator.save(&document.disk_revision, &draft)
        else {
            panic!("{format:?} save failed")
        };
        assert_eq!(saved.format, format);
        assert_eq!(fs::read_to_string(&path).unwrap(), saved.config_preview);
        assert_eq!(loaded(coordinator.load()).normalized_config, draft);
    }
}

#[test]
fn typed_save_rejects_a_compositional_root_without_flattening_it() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("oxiroute.hocon");
    let source = b"templates = { empty = { listeners = [] } }\nuse = \"empty\"\nversion = 1\n";
    fs::write(&path, source).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
    let document = loaded(coordinator.load());

    assert!(document.compositional);
    let ConfigSaveOutcome::InvalidDraft(rejection) =
        coordinator.save(&document.disk_revision, &minimal_config())
    else {
        panic!("compositional root was flattened")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_COMPOSITIONAL_ROOT");
    assert_eq!(fs::read(&path).unwrap(), source);
}

#[test]
fn save_escapes_values_and_installs_a_secure_regular_file() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let expected = loaded(coordinator.load()).disk_revision;
    let draft = rtmp_config("edge \"quoted\" \\ slash café");

    let ConfigSaveOutcome::Saved(saved) = coordinator.save(&expected, &draft) else {
        panic!("save failed")
    };

    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes, saved.config_preview.as_bytes());
    assert!(
        saved
            .config_preview
            .contains(r#"name = "edge \"quoted\" \\ slash café","#)
    );
    assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o7777, 0o600);
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_file());
    assert_eq!(loaded(coordinator.load()).normalized_config, draft);
}

#[test]
fn invalid_draft_is_redacted_and_never_touches_disk() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).disk_revision;
    let mut draft = rtmp_config("private-diagnostic-value");
    draft.version = 99;

    let ConfigSaveOutcome::InvalidDraft(rejection) = coordinator.save(&expected, &draft) else {
        panic!("draft was not rejected")
    };

    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(rejection.diagnostics[0].code, "E_INVALID_VALUE");
    assert!(!format!("{:?}", rejection.diagnostics).contains("private-diagnostic-value"));
}

#[test]
fn invalid_external_content_is_reported_without_rewriting_it() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let invalid = b"return { version = secret_external_value }\n";
    fs::write(&path, invalid).unwrap();

    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("invalid external source was loaded")
    };

    assert_eq!(fs::read(&path).unwrap(), invalid);
    assert_eq!(
        rejection.disk_revision,
        Some(ConfigRevision::from_bytes(invalid))
    );
    assert!(!format!("{:?}", rejection.diagnostics).contains("secret_external_value"));
}

#[test]
fn stale_expected_revision_conflicts_without_overwriting_external_change() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let external = render_lua(&rtmp_config("external")).unwrap();
    fs::write(&path, &external).unwrap();

    let ConfigSaveOutcome::Conflict(conflict) =
        coordinator.save(&expected, &rtmp_config("candidate"))
    else {
        panic!("stale save did not conflict")
    };

    assert_eq!(conflict.expected_revision, expected);
    assert_eq!(
        conflict.disk_revision,
        ConfigRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
}

#[test]
fn exchange_point_race_is_detected_and_external_file_is_restored() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let external = render_lua(&rtmp_config("racing-external")).unwrap();
    let raced_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || fs::write(&raced_path, &external).map_err(|_| ()),
        || {},
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("racing save did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        ConfigRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn post_exchange_replacement_conflicts_without_destroying_external_write() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let external = render_lua(&rtmp_config("post-exchange-external")).unwrap();
    let external_path = temp.path().join("external.lua");
    fs::write(&external_path, &external).unwrap();
    let raced_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || Ok(()),
        || fs::rename(&external_path, &raced_path).unwrap(),
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("post-exchange replacement did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        ConfigRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn post_exchange_in_place_write_conflicts_without_rolling_back_external_bytes() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let external = render_lua(&rtmp_config("post-exchange-in-place")).unwrap();
    let raced_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || Ok(()),
        || fs::write(&raced_path, &external).unwrap(),
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("post-exchange in-place write did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        ConfigRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn post_exchange_writer_is_not_destroyed_when_displaced_revision_also_conflicts() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let pre_exchange = render_lua(&rtmp_config("pre-exchange-external")).unwrap();
    let post_exchange = render_lua(&rtmp_config("post-exchange-external")).unwrap();
    let external_path = temp.path().join("external.lua");
    fs::write(&external_path, &post_exchange).unwrap();
    let before_path = path.clone();
    let after_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || fs::write(&before_path, &pre_exchange).map_err(|_| ()),
        || fs::rename(&external_path, &after_path).unwrap(),
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("two-sided exchange race did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        ConfigRevision::from_bytes(post_exchange.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), post_exchange);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn concurrent_saves_allow_exactly_one_revision_winner() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for name in ["first", "second"] {
        let coordinator = coordinator.clone();
        let expected = expected.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            coordinator.save(&expected, &rtmp_config(name))
        }));
    }
    barrier.wait();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ConfigSaveOutcome::Saved(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ConfigSaveOutcome::Conflict(_)))
            .count(),
        1
    );
    let winning_config = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ConfigSaveOutcome::Saved(document) => Some(&document.normalized_config),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        loaded(coordinator.load()).normalized_config,
        *winning_config
    );
    assert_eq!(
        ConfigRevision::from_bytes(&fs::read(path).unwrap()),
        loaded(coordinator.load()).disk_revision
    );
}

#[test]
fn independent_coordinators_serialize_the_complete_save_transaction() {
    let (temp, path, first) = fixture(&minimal_config());
    let second = CanonicalConfigCoordinator::new(&path).unwrap();
    let expected = loaded(first.load()).disk_revision;
    let first_expected = expected.clone();
    let (first_ready_tx, first_ready_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_worker = thread::spawn(move || {
        first.save_inner(
            &first_expected,
            &rtmp_config("first"),
            || {
                first_ready_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                Ok(())
            },
            || {},
            ReplaceControl::default(),
        )
    });
    first_ready_rx.recv().unwrap();

    let (second_started_tx, second_started_rx) = mpsc::channel();
    let (second_done_tx, second_done_rx) = mpsc::channel();
    let second_worker = thread::spawn(move || {
        second_started_tx.send(()).unwrap();
        let outcome = second.save(&expected, &rtmp_config("second"));
        second_done_tx.send(()).unwrap();
        outcome
    });
    second_started_rx.recv().unwrap();
    let overlapped = second_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_ok();
    release_first_tx.send(()).unwrap();

    let first_outcome = first_worker.join().unwrap();
    let second_outcome = second_worker.join().unwrap();
    assert!(
        !overlapped,
        "independent coordinator bypassed the transaction lock"
    );
    assert!(matches!(first_outcome, ConfigSaveOutcome::Saved(_)));
    assert!(matches!(second_outcome, ConfigSaveOutcome::Conflict(_)));
    assert_eq!(
        loaded(CanonicalConfigCoordinator::new(&path).unwrap().load()).normalized_config,
        rtmp_config("first")
    );
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn child_process_save_helper() {
    let Ok(mode) = env::var("OXIROUTE_CONFIG_SAVE_CHILD") else {
        return;
    };
    let path = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_PATH").unwrap());
    let expected = env::var("OXIROUTE_CONFIG_SAVE_EXPECTED")
        .unwrap()
        .parse::<ConfigRevision>()
        .unwrap();
    let result = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_RESULT").unwrap());
    let ready = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_READY").unwrap());
    let release = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_RELEASE").unwrap());
    let coordinator = CanonicalConfigCoordinator::new(path).unwrap();

    let outcome = if mode == "holder" {
        coordinator.save_inner(
            &expected,
            &rtmp_config("child-first"),
            || {
                fs::write(&ready, []).unwrap();
                wait_for_path(&release);
                Ok(())
            },
            || {},
            ReplaceControl::default(),
        )
    } else {
        fs::write(&ready, []).unwrap();
        coordinator.save(&expected, &rtmp_config("child-second"))
    };
    let label = match outcome {
        ConfigSaveOutcome::Saved(_) => "saved",
        ConfigSaveOutcome::Conflict(_) => "conflict",
        ConfigSaveOutcome::InvalidDraft(_) => "invalid",
        ConfigSaveOutcome::Failed(_) => "failed",
    };
    fs::write(result, label).unwrap();
}

#[test]
fn separate_processes_serialize_the_complete_save_transaction() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let first_result = temp.path().join("first.result");
    let first_ready = temp.path().join("first.ready");
    let release = temp.path().join("release");
    let second_result = temp.path().join("second.result");
    let second_ready = temp.path().join("second.ready");

    let mut first = save_child(
        "holder",
        &path,
        &expected,
        &first_result,
        &first_ready,
        &release,
    );
    wait_for_path(&first_ready);
    let mut second = save_child(
        "writer",
        &path,
        &expected,
        &second_result,
        &second_ready,
        &release,
    );
    wait_for_path(&second_ready);
    thread::sleep(Duration::from_millis(100));
    let overlapped = second.try_wait().unwrap().is_some();
    fs::write(&release, []).unwrap();

    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    assert!(!overlapped, "second process bypassed the transaction lock");
    assert_eq!(fs::read_to_string(first_result).unwrap(), "saved");
    assert_eq!(fs::read_to_string(second_result).unwrap(), "conflict");
    assert_eq!(
        loaded(coordinator.load()).normalized_config,
        rtmp_config("child-first")
    );
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn replacing_the_lock_entry_cannot_create_an_overlapping_lock_namespace() {
    let (temp, path, first) = fixture(&minimal_config());
    let second = CanonicalConfigCoordinator::new(&path).unwrap();
    let expected = loaded(first.load()).disk_revision;
    let first_expected = expected.clone();
    let (first_ready_tx, first_ready_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_worker = thread::spawn(move || {
        first.save_inner(
            &first_expected,
            &rtmp_config("first"),
            || {
                first_ready_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                Ok(())
            },
            || {},
            ReplaceControl::default(),
        )
    });
    first_ready_rx.recv().unwrap();

    let Some(lock_path) = lock_entry(temp.path()) else {
        release_first_tx.send(()).unwrap();
        first_worker.join().unwrap();
        panic!("transaction lock entry was not created")
    };
    let replaced_lock = temp.path().join("replaced.lock");
    fs::rename(&lock_path, &replaced_lock).unwrap();
    fs::write(&lock_path, []).unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();

    let (second_done_tx, second_done_rx) = mpsc::channel();
    let second_worker = thread::spawn(move || {
        let outcome = second.save(&expected, &rtmp_config("second"));
        second_done_tx.send(()).unwrap();
        outcome
    });
    let overlapped = second_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_ok();
    release_first_tx.send(()).unwrap();

    let first_outcome = first_worker.join().unwrap();
    let second_outcome = second_worker.join().unwrap();
    assert!(
        !overlapped,
        "replacement created a second active lock namespace"
    );
    let ConfigSaveOutcome::Failed(failure) = first_outcome else {
        panic!("coordinator using the replaced lock did not fail closed")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_LOCK");
    assert!(matches!(second_outcome, ConfigSaveOutcome::Saved(_)));
    assert_eq!(
        loaded(CanonicalConfigCoordinator::new(&path).unwrap().load()).normalized_config,
        rtmp_config("second")
    );
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn transaction_lock_is_mode_restricted_and_never_follows_symlinks() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let ConfigSaveOutcome::Saved(first) = coordinator.save(&expected, &rtmp_config("first")) else {
        panic!("initial save failed")
    };
    let lock_path = lock_entry(temp.path()).expect("transaction lock entry");
    assert_eq!(
        fs::symlink_metadata(&lock_path).unwrap().mode() & 0o7777,
        0o600
    );

    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    let outcome = CanonicalConfigCoordinator::new(&path)
        .unwrap()
        .save(&first.disk_revision, &rtmp_config("wrong-mode"));
    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("permissive transaction lock was accepted")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_LOCK");

    fs::remove_file(&lock_path).unwrap();
    let outside = temp.path().join("outside.lock");
    fs::write(&outside, []).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&outside, &lock_path).unwrap();
    let outcome = CanonicalConfigCoordinator::new(&path)
        .unwrap()
        .save(&first.disk_revision, &rtmp_config("symlink"));
    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("symbolic transaction lock was followed")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_LOCK");
    assert_eq!(
        loaded(CanonicalConfigCoordinator::new(&path).unwrap().load()).normalized_config,
        rtmp_config("first")
    );
}

#[test]
fn symlink_and_special_file_targets_are_rejected_without_following() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let outside = temp.path().join("outside.lua");
    let outside_bytes = render_lua(&rtmp_config("outside")).unwrap();
    fs::write(&outside, &outside_bytes).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();

    let outcome = coordinator.save(&expected, &rtmp_config("candidate"));

    assert!(matches!(outcome, ConfigSaveOutcome::Failed(_)));
    assert_eq!(fs::read_to_string(&outside).unwrap(), outside_bytes);
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("special file was loaded")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_CONFIG_FILE_TYPE");
}

#[test]
fn intermediate_parent_symlinks_are_rejected_without_following() {
    let temp = TempDir::new().unwrap();
    let real_parent = temp.path().join("real");
    let nested_parent = real_parent.join("nested");
    fs::create_dir_all(&nested_parent).unwrap();
    let real_path = nested_parent.join("oxiroute.lua");
    let original = render_lua(&minimal_config()).unwrap();
    fs::write(&real_path, &original).unwrap();
    let linked_parent = temp.path().join("linked");
    symlink(&real_parent, &linked_parent).unwrap();
    let linked_path = linked_parent.join("nested/oxiroute.lua");
    let coordinator = CanonicalConfigCoordinator::new(&linked_path).unwrap();

    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("configuration beneath an intermediate symlink was loaded")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_CONFIG_READ");

    let outcome = coordinator.save(
        &ConfigRevision::from_bytes(original.as_bytes()),
        &rtmp_config("candidate"),
    );
    assert!(matches!(outcome, ConfigSaveOutcome::Failed(_)));
    assert_eq!(fs::read_to_string(real_path).unwrap(), original);
}

#[test]
fn pinned_parent_chain_rejects_an_intermediate_symlink_rebind() {
    let temp = TempDir::new().unwrap();
    let ancestor = temp.path().join("ancestor");
    let nested_parent = ancestor.join("nested");
    fs::create_dir_all(&nested_parent).unwrap();
    let path = nested_parent.join("oxiroute.lua");
    let original = render_lua(&minimal_config()).unwrap();
    fs::write(&path, &original).unwrap();
    let storage = CanonicalStorage::open(&path).unwrap();

    let moved_ancestor = temp.path().join("moved");
    fs::rename(&ancestor, &moved_ancestor).unwrap();
    symlink(&moved_ancestor, &ancestor).unwrap();

    assert_eq!(storage.read(), Err(StorageFailure::DirectoryChanged));
    assert_eq!(
        fs::read_to_string(moved_ancestor.join("nested/oxiroute.lua")).unwrap(),
        original
    );
}

#[test]
fn oversized_and_unstable_reads_are_rejected() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oxiroute.lua");
    fs::write(&path, vec![b'x'; MAX_CANONICAL_CONFIG_BYTES + 1]).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("oversized file was loaded")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_CONFIG_TOO_LARGE");

    fs::write(&path, b"first").unwrap();
    let storage = CanonicalStorage::open(&path).unwrap();
    let changed_path = path.clone();
    let result = storage.read_with_hook(|| fs::write(changed_path, b"second").unwrap());
    assert_eq!(result, Err(StorageFailure::Unstable));
}

#[test]
fn commit_sync_failure_rolls_back_to_the_old_file() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).disk_revision;

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_commit_sync: true,
            ..ReplaceControl::default()
        },
    );

    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("sync failure did not fail the save")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_DIRECTORY_SYNC");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn committed_cleanup_sync_failure_returns_saved_with_a_warning() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).disk_revision;
    let candidate = rtmp_config("candidate");
    let candidate_bytes = render_lua(&candidate).unwrap();

    let outcome = coordinator.save_inner(
        &expected,
        &candidate,
        || Ok(()),
        || {},
        ReplaceControl {
            fail_cleanup_sync: true,
            ..ReplaceControl::default()
        },
    );

    let ConfigSaveOutcome::Saved(saved) = outcome else {
        panic!("committed cleanup sync failure was reported as unwritten")
    };
    assert_eq!(
        saved.disk_revision,
        ConfigRevision::from_bytes(candidate_bytes.as_bytes())
    );
    assert_eq!(saved.diagnostics.len(), 1);
    assert_eq!(saved.diagnostics[0].code, "W_CONFIG_CLEANUP_DURABILITY");
    assert_eq!(
        saved.diagnostics[0].severity,
        ConfigDiagnosticSeverity::Warning
    );
    assert_eq!(saved.diagnostics[0].stage, ConfigDiagnosticStage::Sync);
    assert_eq!(fs::read_to_string(path).unwrap(), candidate_bytes);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn rollback_cleanup_sync_failure_is_reported() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).disk_revision;

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_commit_sync: true,
            fail_cleanup_sync: true,
            ..ReplaceControl::default()
        },
    );

    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("rollback cleanup sync failure did not fail the save")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_ROLLBACK");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn failure_after_temp_sync_leaves_old_file_and_no_temp_entry() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).disk_revision;

    let outcome = coordinator.save_inner(
        &expected,
        &rtmp_config("candidate"),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_before_exchange: true,
            ..ReplaceControl::default()
        },
    );

    assert!(matches!(outcome, ConfigSaveOutcome::Failed(_)));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn revision_parser_accepts_hex_case_and_rejects_non_sha256_values() {
    let lower = "a3".repeat(32);
    let upper = lower.to_ascii_uppercase();
    assert_eq!(
        lower.parse::<ConfigRevision>().unwrap(),
        upper.parse::<ConfigRevision>().unwrap()
    );
    assert!("not-a-revision".parse::<ConfigRevision>().is_err());
}
