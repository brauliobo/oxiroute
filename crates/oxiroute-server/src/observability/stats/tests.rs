use std::{fs, os::unix::fs::PermissionsExt as _};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, HttpVersionPolicy, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpCapabilities, RtmpPrepareContext, RtmpPrepareMode,
};
use tempfile::TempDir;

use super::*;

fn rtmp_control(_capabilities: RtmpCapabilities) -> RtmpControlHandle {
    PreparedRtmpRuntimeSet::prepare(
        [],
        &RtmpPrepareContext::new(RtmpPrepareMode::Activation, []),
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .expect("empty RTMP preparation")
    .start(std::time::Instant::now() + std::time::Duration::from_secs(1))
    .expect("empty RTMP runtime set")
    .control()
}

fn api() -> (
    HaproxyStatsApi,
    Arc<RoundRobinPool>,
    TempDir,
    String,
    String,
) {
    let directory = TempDir::new().expect("directory");
    let token = "a".repeat(64);
    let token_path = directory.path().join("stats.token");
    fs::write(&token_path, format!("{token}\n")).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let config_path = directory.path().join("oxiroute.lua");
    let config = ConfigDraft {
        listener_workers: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: vec![UpstreamPool {
            name: "pool".into(),
            servers: vec![UpstreamServer {
                name: "origin".into(),
                endpoint: UpstreamEndpoint::Socket {
                    address: "127.0.0.1:3000".parse().expect("address"),
                },
                max_connections: None,
                dns_resolution: DnsResolutionPolicy::OnConnect,
            }],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Safe,
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
        audit: None,
        log_filter: None,
    };
    let config = config.validate().expect("valid config");
    fs::write(
        &config_path,
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
            .expect("render"),
    )
    .expect("config");
    let coordinator = crate::config::coordinator::CanonicalConfigCoordinator::new(config_path)
        .expect("coordinator");
    let crate::config::coordinator::ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("load config")
    };
    let generations = GenerationManager::new();
    let candidate = generations.prepare(*document).expect("prepare");
    let active = generations.activate(&candidate).expect("activate");
    let revision = active.revision().candidate.as_str().to_owned();
    let pool = Arc::clone(&active.pools()[0]);
    let api = HaproxyStatsApi::new(
        active.metrics().clone(),
        vec![Arc::clone(&pool)],
        rtmp_control(active.plan().rtmp_capabilities),
        generations,
        Some(&token_path),
    )
    .expect("stats API");
    (api, pool, directory, token, revision)
}

#[test]
fn sensitive_reads_and_admin_require_loopback_token_and_revision() {
    let (api, pool, _directory, token, revision) = api();
    let authorization = format!("Bearer {token}");
    assert_eq!(api.handle("GET", "/stats", false, None, None).status, 401);
    assert_eq!(
        api.handle("GET", "/stats", true, Some(authorization.as_bytes()), None,)
            .status,
        200
    );
    assert_eq!(api.handle("GET", "/metrics", false, None, None).status, 200);
    assert_eq!(api.handle("GET", "/ready", false, None, None).status, 200);
    assert_eq!(
        api.handle("GET", "/api/v1/status", false, None, None)
            .status,
        401
    );
    assert_eq!(
        api.handle(
            "GET",
            "/api/v1/status",
            true,
            Some(authorization.as_bytes()),
            None,
        )
        .status,
        200
    );
    let target = StatsAdminTarget {
        pool: "pool".into(),
        server: "origin".into(),
        action: "disable".into(),
    };
    assert_eq!(
        api.handle("GET", "/stats/admin", true, None, None).status,
        405
    );
    assert_eq!(api.admin_response(&target, false, None, None).status, 401);
    assert_eq!(
        api.admin_response(&target, true, Some(authorization.as_bytes()), None,)
            .status,
        428
    );
    assert_eq!(
        api.admin_response(
            &target,
            true,
            Some(authorization.as_bytes()),
            Some(&revision),
        )
        .status,
        204
    );
    assert_eq!(
        pool.health_snapshot().endpoints[0].administrative_state,
        crate::AdministrativeState::Maintenance
    );
}

#[test]
fn absent_admin_token_keeps_restricted_stats_routes_closed() {
    let control = rtmp_control(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    });
    let api = HaproxyStatsApi::new(
        RuntimeMetrics::new(),
        Vec::new(),
        control,
        GenerationManager::new(),
        None,
    )
    .expect("stats API without token");

    assert_eq!(api.handle("GET", "/metrics", false, None, None).status, 200);
    assert_eq!(api.handle("GET", "/ready", false, None, None).status, 503);
    for path in ["/", "/stats", "/api/v1/status"] {
        assert_eq!(
            api.handle("GET", path, true, Some(b"Bearer ignored"), None)
                .status,
            401,
            "{path} must remain closed without a configured token"
        );
    }
}

#[test]
fn html_escaping_covers_text_and_attribute_delimiters() {
    let mut output = String::new();
    escape_html(&mut output, "<&>\"'");
    assert_eq!(output, "&lt;&amp;&gt;&quot;&#39;");
}

#[test]
fn stats_transport_loopback_accepts_ipv4_mapped_loopback_only() {
    assert!(transport_ip_is_loopback(
        "::ffff:127.0.0.1".parse().unwrap()
    ));
    assert!(!transport_ip_is_loopback(
        "::ffff:192.0.2.1".parse().unwrap()
    ));

    let (api, _pool, _directory, token, _revision) = api();
    let authorization = format!("Bearer {token}");
    assert!(api.authorized(
        transport_ip_is_loopback("::ffff:127.0.0.1".parse().unwrap()),
        Some(authorization.as_bytes())
    ));
}
