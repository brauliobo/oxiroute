use std::time::Duration;

use oxiroute_config::{
    DnsResolutionPolicy, HttpVersionPolicy, Stats, StatsPage, StatsPageAdminPolicy,
    UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use crate::config_support::empty_config;
use crate::http_support::{self, http_request, raw_http_request};
use crate::process_support::{ServerProcess, reserve_tcp_address, write_token};
use crate::shared::{TOKEN, persistent_request, public_pool};

const STATS_CLIENT_TIMEOUT_MS: u64 = 10_000;

const STATS_REQUEST_TIMEOUT_MS: u64 = 1_000;

const STATS_KEEPALIVE_TIMEOUT_MS: u64 = 5_000;

/// Far above the server-side timeouts, so only a missing timeout fails the observation.
const STATS_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn built_process_serves_readiness_status_and_redacted_metrics_on_multiple_stats_binds() {
    let ipv4_first = reserve_tcp_address();
    let ipv4_second = reserve_tcp_address();
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: vec![ipv4_first, ipv4_second],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        None,
    );
    server.wait_for_tcp(ipv4_first).await;
    server.wait_for_tcp(ipv4_second).await;

    let ready = http_request(ipv4_first, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200);
    assert_eq!(ready.json()["ready"], true);
    assert_eq!(
        http_request(ipv4_second, "GET", "/api/v1/status", &[], &[])
            .await
            .status,
        401
    );
    let summary = http_request(ipv4_second, "GET", "/status.json", &[], &[]).await;
    assert_eq!(summary.status, 200);
    let summary = summary.json();
    assert_eq!(summary["state"], "ok");
    assert_eq!(summary["generation"]["degraded"], false);
    assert_eq!(summary["overdueRequests"], 0);
    assert!(summary["pools"].as_array().expect("pools").is_empty());
    let metrics = http_request(ipv4_first, "GET", "/metrics", &[], &[]).await;
    assert_eq!(metrics.status, 200);
    let metrics = String::from_utf8(metrics.body).expect("metrics UTF-8");
    assert!(metrics.contains("oxiroute_generation_activations_total 1"));
    assert!(metrics.contains("oxiroute_http_inflight_overdue 0"));
    assert!(metrics.contains("oxiroute_generation_info{generation=\"1\",revision=\""));
    assert!(metrics.contains("oxiroute_generation_active_age_seconds"));
    assert!(!metrics.contains(&ipv4_first.to_string()));
    assert!(!metrics.contains(&ipv4_second.to_string()));

    server.shutdown();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one wire test verifies route isolation and the complete secure form flow"
)]
async fn page_only_stats_bind_exposes_only_the_page_and_loopback_form_admin() {
    let page_address = reserve_tcp_address();
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: page_address,
            uri_prefix: "/haproxy".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Localhost,
            max_connections: Some(1),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy {
                client_timeout_ms: Some(STATS_CLIENT_TIMEOUT_MS),
                request_timeout_ms: Some(STATS_REQUEST_TIMEOUT_MS),
                keepalive_timeout_ms: Some(STATS_KEEPALIVE_TIMEOUT_MS),
            },
        }],
    });
    config.upstream_pools = vec![public_pool()];
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        None,
    );
    let mut first = server.connect_when_bound(page_address).await;
    let page = http_support::http_request_on(&mut first, "GET", "/haproxy", &[], &[]).await;
    assert_eq!(page.status, 200);
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert!(page.header("content-security-policy").is_some());
    let get_content_length = page
        .header("content-length")
        .expect("GET content length")
        .to_owned();
    let policy = page.header("content-security-policy").map(str::to_owned);
    let html = String::from_utf8(page.body).expect("stats page HTML");
    assert!(html.contains("<td>public</td><td>origin-a</td>"));
    let revision = html
        .split("name=generation_revision value=\"")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .expect("generation revision");

    for path in [
        "/metrics",
        "/ready",
        "/status.json",
        "/api/v1/status",
        "/stats",
    ] {
        assert_eq!(
            http_request(page_address, "GET", path, &[], &[])
                .await
                .status,
            404,
            "{path}"
        );
    }
    let head = http_request(page_address, "HEAD", "/haproxy", &[], &[]).await;
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(get_content_length.as_str())
    );
    assert_eq!(
        policy.as_deref(),
        Some(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
             img-src 'self' data:; font-src 'self'; form-action 'self'; base-uri 'none'; \
             frame-ancestors 'none'"
        )
    );

    let snapshot = http_request(page_address, "GET", "/haproxy/snapshot.json", &[], &[]).await;
    assert_eq!(snapshot.status, 200);
    assert_eq!(snapshot.header("content-type"), Some("application/json"));
    assert_eq!(snapshot.header("cache-control"), Some("no-store"));
    let snapshot_length = snapshot.header("content-length").map(str::to_owned);
    let snapshot = snapshot.json();
    assert_eq!(snapshot["schemaVersion"], 1);
    assert_eq!(snapshot["admin"], true);
    assert_eq!(snapshot["generation"]["activeRevision"], revision);
    assert_eq!(snapshot["pools"][0]["name"], "public");
    assert_eq!(snapshot["pools"][0]["servers"][0]["name"], "origin-a");
    let snapshot_head =
        http_request(page_address, "HEAD", "/haproxy/snapshot.json", &[], &[]).await;
    assert!(snapshot_head.body.is_empty());
    assert_eq!(
        snapshot_head.header("content-length"),
        snapshot_length.as_deref()
    );
    let csv = http_request(page_address, "GET", "/haproxy/stats.csv", &[], &[]).await;
    assert_eq!(csv.status, 200);
    assert_eq!(csv.header("content-type"), Some("text/csv; charset=utf-8"));
    let csv = String::from_utf8(csv.body().to_vec()).expect("CSV UTF-8");
    assert!(csv.starts_with("# pxname,svname,"), "{csv}");
    assert!(csv.contains("\npublic,origin-a,"), "{csv}");
    for path in ["/haproxy/other", "/haproxyfoo", "/haproxy/snapshot.json/x"] {
        assert_eq!(
            http_request(page_address, "GET", path, &[], &[])
                .await
                .status,
            404,
            "{path}"
        );
    }

    let form = format!("generation_revision={revision}&pool=public&server=origin-a&state=drain");
    let denied = http_request(
        page_address,
        "POST",
        "/haproxy",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        form.as_bytes(),
    )
    .await;
    assert_eq!(denied.status, 403);
    let rebound = raw_http_request(
        page_address,
        b"POST /haproxy HTTP/1.1\r\nHost: attacker.test\r\nOrigin: http://attacker.test\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(rebound.status, 403);
    let forwarded = http_request(
        page_address,
        "POST",
        "/haproxy",
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Origin", "http://localhost"),
            ("Forwarded", "for=127.0.0.1"),
        ],
        form.as_bytes(),
    )
    .await;
    assert_eq!(forwarded.status, 403);
    let oversized_form = vec![b'a'; 8 * 1024 + 1];
    let oversized = http_request(
        page_address,
        "POST",
        "/haproxy",
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Origin", "http://localhost"),
        ],
        &oversized_form,
    )
    .await;
    assert_eq!(oversized.status, 413);
    let drained = http_request(
        page_address,
        "POST",
        "/haproxy",
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Origin", "http://localhost"),
        ],
        form.as_bytes(),
    )
    .await;
    assert_eq!(drained.status, 204, "{}", drained.text());
    assert!(
        http_request(page_address, "GET", "/haproxy", &[], &[])
            .await
            .text()
            .contains("<td>drain</td>")
    );

    let ready_form = form.replace("state=drain", "state=ready");
    let referer_fallback = http_request(
        page_address,
        "POST",
        "/haproxy",
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Referer", "http://localhost/haproxy"),
        ],
        ready_form.as_bytes(),
    )
    .await;
    assert_eq!(referer_fallback.status, 204, "{}", referer_fallback.text());

    // The answered keep-alive request proves the server admitted this connection, so the next one
    // is deterministically the one above the cap.
    let mut stalled = TcpStream::connect(page_address)
        .await
        .expect("held stats connection");
    assert_eq!(
        persistent_request(&mut stalled, "GET", "/haproxy", &[], &[])
            .await
            .status,
        200
    );
    stalled
        .write_all(b"GET /haproxy HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("partial stats request");
    let mut rejected = TcpStream::connect(page_address)
        .await
        .expect("connection above stats cap");
    rejected
        .write_all(b"GET /haproxy HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("request above stats cap");
    let mut rejected_bytes = Vec::new();
    match tokio::time::timeout(
        STATS_OBSERVATION_TIMEOUT,
        rejected.read_to_end(&mut rejected_bytes),
    )
    .await
    .expect("capped stats connection remained open")
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("read capped stats connection: {error}"),
    }
    assert!(rejected_bytes.is_empty());
    let mut stalled_bytes = Vec::new();
    match tokio::time::timeout(
        STATS_OBSERVATION_TIMEOUT,
        stalled.read_to_end(&mut stalled_bytes),
    )
    .await
    .expect("stats request timeout was not enforced")
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("read timed-out stats connection: {error}"),
    }
    assert!(stalled_bytes.is_empty());
    assert_eq!(
        http_request(page_address, "GET", "/haproxy", &[], &[])
            .await
            .status,
        200
    );

    server.shutdown();
}

#[tokio::test]
async fn stats_admin_uses_json_targets_and_rejects_duplicate_authorization_headers() {
    let directory = TempDir::new().expect("stats admin directory");
    let token_path = write_token(directory.path(), TOKEN, 0o600);
    let stats_address = reserve_tcp_address();
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: Some(token_path),
        pages: Vec::new(),
    });
    config.upstream_pools = vec![UpstreamPool {
        name: "public".into(),
        servers: vec![UpstreamServer {
            name: "origin-a".into(),
            endpoint: UpstreamEndpoint::Socket {
                address: "127.0.0.1:8080".parse().expect("upstream"),
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
    }];
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        None,
    );
    server.wait_for_tcp(stats_address).await;
    let authorization = format!("Bearer {TOKEN}");

    let duplicate = http_request(
        stats_address,
        "GET",
        "/stats",
        &[
            ("Authorization", &authorization),
            ("Authorization", &authorization),
        ],
        &[],
    )
    .await;
    assert_eq!(duplicate.status, 400);
    assert_eq!(duplicate.json()["error"]["code"], "duplicate_authorization");

    let revision = http_request(
        stats_address,
        "GET",
        "/api/v1/status",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("active stats revision")
        .to_owned();
    let disabled = http_request(
        stats_address,
        "POST",
        "/stats/admin",
        &[
            ("Authorization", &authorization),
            ("If-Generation-Revision", &revision),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_vec(&json!({
            "pool": "public",
            "server": "origin-a",
            "action": "disable",
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(disabled.status, 204, "{}", disabled.text());
    let stats = http_request(
        stats_address,
        "GET",
        "/stats",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(stats.status, 200);
    assert!(String::from_utf8_lossy(&stats.body).contains("maintenance"));

    server.shutdown();
}
