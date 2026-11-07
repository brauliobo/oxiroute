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
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
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
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    };
    let config = config.validate().expect("valid config");
    fs::write(
        &config_path,
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
            .expect("render"),
    )
    .expect("config");
    let coordinator = crate::config_coordinator::CanonicalConfigCoordinator::new(config_path)
        .expect("coordinator");
    let crate::config_coordinator::ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
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
fn page_routes_are_public_read_only_and_isolated_from_protected_apis() {
    let (api, _pool, _directory, _token, _revision) = api();
    let config = StatsPage {
        bind: "127.0.0.1:8404".parse().expect("bind"),
        uri_prefix: "/haproxy".into(),
        refresh_ms: 2_500,
        admin: StatsPageAdminPolicy::Localhost,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    };
    let page = HaproxyStatsPage::new(&config, api.pools.clone(), api.generations.clone());

    let remote = page.handle("GET", "/haproxy", false);
    let remote_body = String::from_utf8(remote.body).expect("HTML");
    assert_eq!(remote.status, 200);
    assert!(remote_body.contains("name=oxiroute-refresh-ms content=\"2500\""));
    assert!(remote_body.contains("<td>pool</td><td>origin</td>"));
    assert!(!remote_body.contains("<form"));
    assert_eq!(page.handle("GET", "/haproxy/servers", false).status, 200);
    assert_eq!(page.handle("GET", "/haprox", false).status, 404);

    let local = page.handle("GET", "/haproxy", true);
    assert!(
        String::from_utf8(local.body)
            .unwrap()
            .contains("<form method=post")
    );
    assert!(page.handle("HEAD", "/haproxy", true).body.is_empty());
    for path in ["/metrics", "/ready", "/api/v1/status", "/stats"] {
        assert_eq!(page.handle("GET", path, true).status, 404, "{path}");
    }
    assert_eq!(page.handle("PUT", "/haproxy", true).status, 405);

    let secured = secure_page_response(page.handle("GET", "/haproxy", false));
    assert_eq!(secured.headers()["cache-control"], "no-store");
    assert_eq!(secured.headers()["x-content-type-options"], "nosniff");
    assert!(secured.headers().contains_key("content-security-policy"));
}

#[test]
fn localhost_page_admin_requires_origin_and_revision_and_supports_all_states() {
    let (api, pool, _directory, _token, revision) = api();
    let config = StatsPage {
        bind: "127.0.0.1:8404".parse().expect("bind"),
        uri_prefix: "/haproxy".into(),
        refresh_ms: 10_000,
        admin: StatsPageAdminPolicy::Localhost,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    };
    let page = HaproxyStatsPage::new(&config, api.pools.clone(), api.generations.clone());
    let mut target = StatsPageAdminTarget {
        generation_revision: revision,
        pool: "pool".into(),
        server: "origin".into(),
        state: "drain".into(),
    };

    assert_eq!(page.admin_response(&target, false, true).status, 403);
    assert_eq!(page.admin_response(&target, true, false).status, 403);
    assert_eq!(page.admin_response(&target, true, true).status, 204);
    assert_eq!(
        pool.health_snapshot().endpoints[0].administrative_state,
        crate::AdministrativeState::Drain
    );
    target.state = "maintenance".into();
    assert_eq!(page.admin_response(&target, true, true).status, 204);
    target.state = "ready".into();
    assert_eq!(page.admin_response(&target, true, true).status, 204);
    assert_eq!(
        pool.health_snapshot().endpoints[0].administrative_state,
        crate::AdministrativeState::Ready
    );
    target.generation_revision = "stale".into();
    let response = page.admin_response(&target, true, true);
    assert_eq!(response.status, 409);
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["code"], "generation_conflict");
}

#[test]
fn stats_page_form_and_same_origin_checks_fail_closed() {
    let target = parse_stats_form(
        b"generation_revision=abc&pool=pool%20one&server=node%2B1&state=maintenance",
    )
    .expect("form");
    assert_eq!(target.pool, "pool one");
    assert_eq!(target.server, "node+1");
    assert!(parse_stats_form(b"pool=one&pool=two").is_err());
    assert!(
        parse_stats_form(b"generation_revision=abc&pool=one&server=two&state=ready&extra=no")
            .is_err()
    );

    let mut headers = HeaderMap::new();
    headers.insert(HOST, HeaderValue::from_static("localhost:8404"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:8404"));
    assert!(request_is_same_origin(&headers));
    headers.remove(ORIGIN);
    headers.insert(
        REFERER,
        HeaderValue::from_static("http://localhost:8404/stats"),
    );
    assert!(request_is_same_origin(&headers));
    headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:8404"));
    headers.insert("forwarded", HeaderValue::from_static("for=127.0.0.1"));
    assert!(!request_is_same_origin(&headers));
    headers.remove("forwarded");
    headers.insert(ORIGIN, HeaderValue::from_static("http://attacker.test"));
    assert!(!request_is_same_origin(&headers));
    headers.insert(HOST, HeaderValue::from_static("attacker.test:8404"));
    headers.insert(
        ORIGIN,
        HeaderValue::from_static("http://attacker.test:8404"),
    );
    assert!(!request_is_same_origin(&headers));
    headers.insert(HOST, HeaderValue::from_static("127.0.0.1:8404"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:8404"));
    assert!(request_is_same_origin(&headers));
    headers.insert(HOST, HeaderValue::from_static("localhost:not-a-port"));
    headers.insert(
        ORIGIN,
        HeaderValue::from_static("http://localhost:not-a-port"),
    );
    assert!(!request_is_same_origin(&headers));
    assert!(authority_is_loopback(
        &"[::1]:8404".parse().expect("IPv6 loopback authority")
    ));
    assert!(!authority_is_loopback(
        &"attacker@localhost:8404"
            .parse()
            .expect("userinfo authority")
    ));
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
