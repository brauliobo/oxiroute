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
    ConfigDraft, HttpHostSelector, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction,
    HttpService, HttpVersionPolicy, Listener, ListenerBind, Protocol, RtmpApplication, RtmpService,
    UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
};
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, load_lua, render_config, resolve_source_with_format,
};
use tempfile::TempDir;

use super::{storage::StorageFailure, *};

mod cache;
mod concurrency;
mod filesystem;
mod load;
mod save;

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

fn minimal_config() -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
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

fn rtmp_config(name: &str) -> ConfigDraft {
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
        max_inbound_message_size: 8 * 1024 * 1024,
        ack_window_size: 5_000_000,
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

fn normalizable_config() -> ConfigDraft {
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
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    config.http_services.push(HttpService {
        drain_deadline_ms: None,
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
                policy: HttpProxyPolicy::default().into(),
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

fn fixture(config: &ConfigDraft) -> (TempDir, std::path::PathBuf, CanonicalConfigCoordinator) {
    let temp = TempDir::new().expect("temporary directory");
    let path = temp.path().join("oxiroute.lua");
    let config = config.clone().validate().expect("test config validates");
    fs::write(
        &path,
        render_config(ConfigFormat::Lua, &config).expect("test config renders"),
    )
    .expect("write config");
    let coordinator = CanonicalConfigCoordinator::new(path.clone()).expect("coordinator");
    (temp, path, coordinator)
}

fn loaded(outcome: ConfigLoadOutcome) -> ResolvedConfigDocument {
    let ConfigLoadOutcome::Loaded(document) = outcome else {
        panic!("load rejected")
    };
    *document
}

fn prepared(
    coordinator: &CanonicalConfigCoordinator,
    draft: ConfigDraft,
) -> PersistableConfigCandidate {
    let ConfigValidationOutcome::Valid(candidate) = coordinator.prepare(draft) else {
        panic!("draft is invalid")
    };
    *candidate
}

fn save_draft(
    coordinator: &CanonicalConfigCoordinator,
    expected: &AuthoredRevision,
    draft: ConfigDraft,
) -> ConfigSaveOutcome {
    coordinator.save(expected, prepared(coordinator, draft))
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
    let deadline = Instant::now() + Duration::from_secs(30);
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
    expected: &AuthoredRevision,
    result: &Path,
    ready: &Path,
    release: &Path,
) -> Child {
    Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("config::coordinator::tests::child_process_save_helper")
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
fn child_process_save_helper() {
    let Ok(mode) = env::var("OXIROUTE_CONFIG_SAVE_CHILD") else {
        return;
    };
    let path = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_PATH").unwrap());
    let expected = env::var("OXIROUTE_CONFIG_SAVE_EXPECTED")
        .unwrap()
        .parse::<AuthoredRevision>()
        .unwrap();
    let result = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_RESULT").unwrap());
    let ready = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_READY").unwrap());
    let release = PathBuf::from(env::var_os("OXIROUTE_CONFIG_SAVE_RELEASE").unwrap());
    let coordinator = CanonicalConfigCoordinator::new(path).unwrap();

    let outcome = if mode == "holder" {
        coordinator.save_inner(
            &expected,
            prepared(&coordinator, rtmp_config("child-first")),
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
        save_draft(&coordinator, &expected, rtmp_config("child-second"))
    };
    let label = match outcome {
        ConfigSaveOutcome::Saved(_) => "saved",
        ConfigSaveOutcome::Conflict(_) => "conflict",
        ConfigSaveOutcome::InvalidDraft(_) => "invalid",
        ConfigSaveOutcome::Failed(_) => "failed",
    };
    fs::write(result, label).unwrap();
}
