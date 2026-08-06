#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    time::{Duration, Instant},
};

use oxiroute_config::{
    Config, DnsResolutionPolicy, HttpVersionPolicy, Listener, ListenerBind, Management, Protocol,
    RtmpApplication, RtmpRecorderStart, RtmpService, Stats, StatsPage, StatsPageAdminPolicy,
    UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use config_support::{empty_config, rtmp_recorder_with_queue_bytes, socket_bind};
use http_support::{http_request, raw_http_request};
use process_support::{
    ServerProcess, build_ui, output_text, reserve_tcp_address, run_to_failure, write_config,
    write_token,
};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

#[test]
fn sigterm_during_runtime_startup_is_bounded() {
    let ui = TempDir::new().expect("startup UI directory");
    fs::write(ui.path().join("index.html"), "<!doctype html>").expect("index");
    fs::create_dir(ui.path().join("assets")).expect("assets");
    for index in 0..256 {
        fs::write(ui.path().join("assets").join(format!("{index}.js")), b"x").expect("asset");
    }
    let config = management_config(reserve_tcp_address(), Some(ui.path().to_path_buf()));
    let server = ServerProcess::start(&config, Some(TOKEN));
    std::thread::sleep(Duration::from_millis(10));
    let started = Instant::now();

    server.shutdown();

    assert!(started.elapsed() < Duration::from_secs(6));
}

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
    let mut server = ServerProcess::start(&config, None);
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
    let metrics = http_request(ipv4_first, "GET", "/metrics", &[], &[]).await;
    assert_eq!(metrics.status, 200);
    let metrics = String::from_utf8(metrics.body).expect("metrics UTF-8");
    assert!(metrics.contains("oxiroute_generation_activations_total 1"));
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
                client_timeout_ms: Some(1_000),
                request_timeout_ms: Some(150),
                keepalive_timeout_ms: Some(100),
            },
        }],
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
    }];
    let mut server = ServerProcess::start(&config, None);
    server.wait_for_tcp(page_address).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let page = http_request(page_address, "GET", "/haproxy", &[], &[]).await;
    assert_eq!(page.status, 200);
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert!(page.header("content-security-policy").is_some());
    let get_content_length = page
        .header("content-length")
        .expect("GET content length")
        .to_owned();
    let html = String::from_utf8(page.body).expect("stats page HTML");
    assert!(html.contains("<td>public</td><td>origin-a</td>"));
    let revision = html
        .split("name=generation_revision value=\"")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .expect("generation revision");

    for path in ["/metrics", "/ready", "/api/v1/status", "/stats"] {
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

    let mut stalled = TcpStream::connect(page_address)
        .await
        .expect("held stats connection");
    stalled
        .write_all(b"GET /haproxy HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("partial stats request");
    tokio::time::sleep(Duration::from_millis(25)).await;
    let mut rejected = TcpStream::connect(page_address)
        .await
        .expect("connection above stats cap");
    rejected
        .write_all(b"GET /haproxy HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("request above stats cap");
    let mut rejected_bytes = Vec::new();
    match tokio::time::timeout(
        Duration::from_secs(1),
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
        Duration::from_secs(1),
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

#[tokio::test(flavor = "current_thread")]
async fn generation_status_remains_responsive_through_publication() {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let mut status_connection = TcpStream::connect(management_address)
        .await
        .expect("status connection");
    let original_status = tokio::time::timeout(
        Duration::from_secs(1),
        persistent_request(
            &mut status_connection,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        ),
    )
    .await
    .expect("initial generation status timed out");
    let original_revision = original_status.json()["generation"]["activeRevision"]
        .as_str()
        .expect("original revision")
        .to_owned();

    let reader_authorization = authorization.clone();
    let reader_revision = original_revision.clone();
    let (request_started, mut request_start) = tokio::sync::mpsc::unbounded_channel();
    let status_reader = tokio::spawn(async move {
        let mut status_reads = 0_u64;
        loop {
            request_started.send(()).expect("status reader receiver");
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                persistent_request(
                    &mut status_connection,
                    "GET",
                    "/api/v1/generations",
                    &[("Authorization", &reader_authorization)],
                    &[],
                ),
            )
            .await
            .expect("generation status blocked during publication");
            assert_eq!(status.status, 200);
            status_reads += 1;
            if status.json()["generation"]["activeRevision"].as_str()
                != Some(reader_revision.as_str())
            {
                return status_reads;
            }
            tokio::task::yield_now().await;
        }
    });
    request_start
        .recv()
        .await
        .expect("first status request started");
    config.max_connections = Some(101);
    write_config(&server.config_path, &config);
    let status_reads = tokio::time::timeout(process_support::PROCESS_TIMEOUT, status_reader)
        .await
        .expect("generation reload timed out")
        .expect("status reader");
    assert!(status_reads > 1, "status was not sampled continuously");

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
    }];
    let mut server = ServerProcess::start(&config, None);
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn persistent_old_generation_management_and_stats_connections_mutate_the_selected_generation()
{
    let directory = TempDir::new().expect("persistent generation directory");
    let stats_token = write_token(directory.path(), TOKEN, 0o600);
    let management_address = reserve_tcp_address();
    let stats_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    config.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: Some(stats_token),
        pages: Vec::new(),
    });
    config.upstream_pools = vec![UpstreamPool {
        name: "public".into(),
        servers: vec![UpstreamServer {
            name: "origin-a".into(),
            endpoint: UpstreamEndpoint::Socket {
                address: "127.0.0.1:8080".parse().expect("upstream"),
            },
            max_connections: Some(10),
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
    }];
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(stats_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let mut old_management = TcpStream::connect(management_address)
        .await
        .expect("old management connection");
    let mut old_stats = TcpStream::connect(stats_address)
        .await
        .expect("old stats connection");
    let original_revision = persistent_request(
        &mut old_management,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("original revision")
        .to_owned();
    assert_eq!(
        persistent_request(
            &mut old_stats,
            "GET",
            "/api/v1/status",
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .status,
        200
    );

    config.max_connections = Some(101);
    write_config(&server.config_path, &config);
    let deadline = Instant::now() + process_support::PROCESS_TIMEOUT;
    let active_revision = loop {
        let response = http_request(
            management_address,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        )
        .await;
        if let Some(revision) = response.json()["generation"]["activeRevision"].as_str() {
            if revision != original_revision {
                break revision.to_owned();
            }
        }
        assert!(Instant::now() < deadline, "generation reload timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let capacity = serde_json::to_vec(&json!({
        "targets": [{ "pool": "public", "server": "origin-a" }],
        "maxConnections": 3,
        "expectedActiveRevision": active_revision,
    }))
    .unwrap();
    assert_eq!(
        persistent_request(
            &mut old_management,
            "PUT",
            "/api/v1/servers/max-connections",
            &[
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
            ],
            &capacity,
        )
        .await
        .status,
        200
    );
    let active_servers = http_request(
        management_address,
        "GET",
        "/api/v1/servers",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert_eq!(active_servers["servers"][0]["server"]["maxConnections"], 3);

    let admin = serde_json::to_vec(&json!({
        "pool": "public",
        "server": "origin-a",
        "action": "disable",
    }))
    .unwrap();
    assert_eq!(
        persistent_request(
            &mut old_stats,
            "POST",
            "/stats/admin",
            &[
                ("Authorization", &authorization),
                ("If-Generation-Revision", &active_revision),
                ("Content-Type", "application/json"),
            ],
            &admin,
        )
        .await
        .status,
        204
    );
    let active_stats = http_request(
        stats_address,
        "GET",
        "/stats",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert!(active_stats.text().contains("maintenance"));

    drop(old_management);
    drop(old_stats);
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn built_process_replaces_generation_and_reuses_existing_listener_reservations() {
    let original_bind = reserve_tcp_address();
    let added_bind = reserve_tcp_address();
    let management_bind = reserve_tcp_address();
    let mut config = empty_config();
    config.management = Some(Management {
        bind: management_bind,
        ui_dir: None,
    });
    config.stats = Some(Stats {
        binds: vec![original_bind],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(original_bind).await;
    server.wait_for_tcp(management_bind).await;
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = http_request(
        management_bind,
        "GET",
        "/api/v1/status",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("original revision")
        .to_owned();

    config.max_connections = Some(100);
    config.stats.as_mut().expect("stats").binds.push(added_bind);
    write_config(&server.config_path, &config);
    server.wait_for_tcp(added_bind).await;
    let deadline = std::time::Instant::now() + process_support::PROCESS_TIMEOUT;
    loop {
        let status = http_request(
            management_bind,
            "GET",
            "/api/v1/status",
            &[("Authorization", &authorization)],
            &[],
        )
        .await;
        if status.status == 200
            && status.json()["activeRevision"].as_str() != Some(original_revision.as_str())
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "generation did not activate"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        http_request(added_bind, "GET", "/ready", &[], &[])
            .await
            .status,
        200
    );

    let active_revision = http_request(
        management_bind,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    let drained = http_request(
        management_bind,
        "POST",
        "/api/v1/process/drain",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_vec(&json!({
            "expectedActiveRevision": active_revision,
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(drained.status, 202);
    config.max_connections = Some(101);
    write_config(&server.config_path, &config);
    let deadline = std::time::Instant::now() + process_support::PROCESS_TIMEOUT;
    loop {
        let status = http_request(
            management_bind,
            "GET",
            "/api/v1/status",
            &[("Authorization", &authorization)],
            &[],
        )
        .await;
        if status.status == 200
            && status.json()["activeRevision"].as_str() != Some(active_revision.as_str())
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "drained reload timed out"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        http_request(management_bind, "GET", "/ready", &[], &[])
            .await
            .status,
        503
    );
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn built_management_ui_and_authenticated_config_lifecycle_run_over_real_tcp() {
    let ui_dir = build_ui();
    let management_address = reserve_tcp_address();
    let active = management_config(management_address, Some(ui_dir.clone()));
    let mut server = ServerProcess::start(&active, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let token_path = server.token_path.as_ref().expect("management token file");
    let authorization = format!("Bearer {TOKEN}");
    assert_eq!(
        fs::metadata(token_path)
            .expect("token metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let index = http_request(management_address, "GET", "/", &[], &[]).await;
    assert_eq!(index.status, 200);
    assert_eq!(
        index.headers.get("content-type").map(String::as_str),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(index.body, fs::read(ui_dir.join("index.html")).unwrap());
    let asset_paths = built_asset_paths(&index.body);
    assert!(!asset_paths.is_empty(), "built index must reference assets");
    for path in asset_paths {
        let response = http_request(management_address, "GET", &path, &[], &[]).await;
        assert_eq!(response.status, 200, "asset {path}");
        assert_eq!(
            response.body,
            fs::read(ui_dir.join(path.trim_start_matches('/'))).unwrap(),
            "asset {path}"
        );
    }

    let monitoring = http_request(
        management_address,
        "GET",
        "/api/v1/monitoring",
        &[
            ("Cache-Control", "no-store"),
            ("Authorization", &authorization),
        ],
        &[],
    )
    .await;
    assert_eq!(monitoring.status, 200);
    let monitoring = monitoring.json();
    assert_eq!(monitoring["listeners"], json!([]));
    assert_eq!(monitoring["upstreamPools"], json!([]));
    assert_eq!(monitoring["certbotCertificates"], json!([]));
    assert!(monitoring["certbotWatcher"].is_null());
    assert_eq!(
        monitoring["rtmp"],
        json!({
            "activeStreams": 0,
            "publishers": 0,
            "subscribers": 0,
            "mediaPayloadBytesReceived": "0",
            "recordingSupported": false,
            "manualRecording": false,
            "recorderBytesWritten": "0",
            "recorderSegmentsStarted": "0",
            "recorderSegmentsCompleted": "0",
            "recorderDiscontinuities": "0",
            "relayConnectionAttempts": "0",
            "relayConnections": "0",
            "relayReconnects": "0",
            "relayDnsRefreshAttempts": "0",
            "relayDnsRefreshSuccesses": "0",
            "relayDnsRefreshFailures": "0",
            "relayEventsSent": "0",
            "relayEventsDropped": "0",
            "relayPayloadBytesSent": "0",
            "accessLog": {
                "queueCapacity": 1_024,
                "queueDepth": "0",
                "enqueued": "0",
                "written": "0",
                "dropped": "0",
                "queueSaturated": "0",
                "writeFailures": "0",
            },
            "relays": [],
            "recorders": [],
        })
    );

    let catalog = http_request(
        management_address,
        "GET",
        "/api/v1/rtmp/streams",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert!(catalog["as_of_unix_ms"].as_u64().is_some());
    assert_eq!(catalog["revision"], "0");
    assert_eq!(
        catalog["capabilities"],
        json!({ "live_ingest": false, "manual_recording": false })
    );
    assert_eq!(catalog["streams"], json!([]));

    let topology = http_request(
        management_address,
        "GET",
        "/api/v1/topology",
        &[
            ("Cache-Control", "no-store"),
            ("Authorization", &authorization),
        ],
        &[],
    )
    .await
    .json();
    assert_eq!(topology["schemaVersion"], 1);
    assert_eq!(topology["state"]["config"], "active");
    assert_eq!(topology["state"]["runtime"], "active");
    assert_eq!(topology["nodes"], json!([]));
    assert_eq!(topology["edges"], json!([]));
    assert_eq!(topology["overlays"], json!([]));

    let unauthorized = http_request(management_address, "GET", "/api/v1/config", &[], &[]).await;
    assert_eq!(unauthorized.status, 401);
    assert_eq!(unauthorized.json()["error"]["code"], "unauthorized");

    let get = http_request(
        management_address,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(get.status, 200);
    let snapshot = get.json();
    assert_eq!(snapshot["schemaVersion"], 1);
    assert_eq!(snapshot["config"], serde_json::to_value(&active).unwrap());
    assert_eq!(snapshot["candidateRevision"], snapshot["activeRevision"]);
    assert_ne!(snapshot["diskRevision"], snapshot["activeRevision"]);
    assert_eq!(snapshot["configFormat"], "lua");
    assert_eq!(snapshot["compositional"], false);
    assert!(!String::from_utf8_lossy(&get.body).contains(TOKEN));

    let missing_root = TempDir::new()
        .expect("secret recording parent")
        .path()
        .join("tenant-secret-recording-root");
    let invalid_candidate = recording_candidate(&active, &missing_root);
    let invalid_body = serde_json::to_vec(&json!({ "config": invalid_candidate })).unwrap();
    let rejected = http_request(
        management_address,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &invalid_body,
    )
    .await;
    assert_eq!(rejected.status, 422);
    assert_eq!(
        rejected.json()["diagnostics"][0]["code"],
        "E_RUNTIME_PREPARE"
    );
    let rejected_wire = String::from_utf8_lossy(&rejected.body);
    assert!(
        rejected_wire.contains("candidate cannot be prepared as a complete runtime generation")
    );
    assert!(!rejected_wire.contains(&missing_root.display().to_string()));
    assert!(!rejected_wire.contains(TOKEN));

    let mut candidate = active.clone();
    candidate.management.as_mut().unwrap().ui_dir = None;
    let candidate_body = serde_json::to_vec(&json!({ "config": candidate })).unwrap();
    let validated = http_request(
        management_address,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &candidate_body,
    )
    .await;
    assert_eq!(validated.status, 200);
    assert_eq!(
        validated.json()["normalizedConfig"],
        serde_json::to_value(&candidate).unwrap()
    );
    assert_eq!(validated.json()["configFormat"], "lua");
    assert_eq!(
        validated.json()["configPreview"],
        validated.json()["luaPreview"]
    );

    let revision = snapshot["diskRevision"].as_str().unwrap();
    let saved = http_request(
        management_address,
        "PUT",
        "/api/v1/config",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
            ("If-Config-Revision", revision),
        ],
        &candidate_body,
    )
    .await;
    assert_eq!(saved.status, 200);
    let saved = saved.json();
    assert_eq!(saved["activeRevision"], snapshot["activeRevision"]);
    assert_eq!(saved["outcome"], "saved_pending_activation");
    assert_eq!(saved["activationState"], "pending");
    assert_eq!(saved["restartRequired"], false);

    let persisted = http_request(
        management_address,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert_eq!(
        persisted["config"],
        serde_json::to_value(candidate).unwrap()
    );
    assert_eq!(persisted["diskRevision"], saved["diskRevision"]);
    assert_eq!(persisted["activeRevision"], snapshot["activeRevision"]);
    assert!(
        fs::read_to_string(&server.config_path)
            .expect("persisted process config")
            .contains("ui_dir = nil")
    );
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authenticated_server_batches_are_prevalidated_and_mutate_owned_runtime_state() {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    config.upstream_pools = ["public-v4", "public-v6"]
        .into_iter()
        .map(|name| UpstreamPool {
            name: name.into(),
            servers: vec![UpstreamServer {
                name: "origin-a".into(),
                endpoint: UpstreamEndpoint::Socket {
                    address: "127.0.0.1:8080".parse().expect("upstream"),
                },
                max_connections: Some(10),
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
        })
        .collect();
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let active_revision = http_request(
        management_address,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();

    let unauthorized = http_request(management_address, "GET", "/api/v1/servers", &[], &[]).await;
    assert_eq!(unauthorized.status, 401);

    let rejected = http_request(
        management_address,
        "POST",
        "/api/v1/servers/administrative-state",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_vec(&json!({
            "targets": [
                { "pool": "public-v4", "server": "origin-a" },
                { "pool": "missing", "server": "origin-a" }
            ],
            "state": "drain",
            "expectedActiveRevision": active_revision.clone(),
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(rejected.status, 404);
    assert_eq!(rejected.json()["error"]["code"], "pool_not_found");

    let stale = http_request(
        management_address,
        "POST",
        "/api/v1/servers/administrative-state",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_vec(&json!({
            "targets": [{ "pool": "public-v4", "server": "origin-a" }],
            "state": "drain",
            "expectedActiveRevision": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(stale.status, 409);
    assert_eq!(stale.json()["error"]["code"], "generation_conflict");

    let unchanged = http_request(
        management_address,
        "GET",
        "/api/v1/servers",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert!(
        unchanged["servers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| { entry["server"]["administrativeState"] == "ready" })
    );

    for (path, mut body) in [
        (
            "/api/v1/servers/administrative-state",
            json!({
                "targets": [
                    { "pool": "public-v4", "server": "origin-a" },
                    { "pool": "public-v6", "server": "origin-a" }
                ],
                "state": "drain"
            }),
        ),
        (
            "/api/v1/servers/health-override",
            json!({
                "targets": [{ "pool": "public-v4", "server": "origin-a" }],
                "health": "down"
            }),
        ),
        (
            "/api/v1/servers/checks",
            json!({
                "targets": [{ "pool": "public-v4", "server": "origin-a" }],
                "enabled": false
            }),
        ),
    ] {
        body["expectedActiveRevision"] = json!(active_revision.clone());
        let response = http_request(
            management_address,
            "POST",
            path,
            &[
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
            ],
            &serde_json::to_vec(&body).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{path}: {}", response.json());
    }
    let capacity = http_request(
        management_address,
        "PUT",
        "/api/v1/servers/max-connections",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_vec(&json!({
            "targets": [{ "pool": "public-v4", "server": "origin-a" }],
            "maxConnections": 3,
            "expectedActiveRevision": active_revision,
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(capacity.status, 200);

    let changed = http_request(
        management_address,
        "GET",
        "/api/v1/servers",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(changed.status, 200);
    assert!(!String::from_utf8_lossy(&changed.body).contains(TOKEN));
    let changed = changed.json();
    let v4 = changed["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pool"] == "public-v4")
        .unwrap();
    assert_eq!(v4["server"]["administrativeState"], "drain");
    assert_eq!(v4["server"]["healthOverride"], "down");
    assert_eq!(v4["server"]["checksEnabled"], false);
    assert_eq!(v4["server"]["maxConnections"], 3);

    server.shutdown();
}

#[tokio::test]
async fn dns_refresh_batch_reports_every_target_and_explicit_non_atomic_outcomes() {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    config.upstream_pools = vec![UpstreamPool {
        name: "dns".into(),
        servers: vec![
            UpstreamServer {
                name: "resolvable".into(),
                endpoint: UpstreamEndpoint::Dns {
                    host: "localhost".into(),
                    port: 8080,
                },
                max_connections: None,
                dns_resolution: DnsResolutionPolicy::OnConnect,
            },
            UpstreamServer {
                name: "missing".into(),
                endpoint: UpstreamEndpoint::Dns {
                    host: "does-not-exist.invalid".into(),
                    port: 8080,
                },
                max_connections: None,
                dns_resolution: DnsResolutionPolicy::OnConnect,
            },
        ],
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
    }];
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");

    let duplicate = http_request(
        management_address,
        "GET",
        "/api/v1/servers",
        &[
            ("Authorization", &authorization),
            ("Authorization", &authorization),
        ],
        &[],
    )
    .await;
    assert_eq!(duplicate.status, 400);
    assert_eq!(duplicate.json()["error"]["code"], "duplicate_authorization");

    let active_revision = http_request(
        management_address,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    let refreshed = http_request(
        management_address,
        "POST",
        "/api/v1/servers/refresh-dns",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_vec(&json!({
            "targets": [
                { "pool": "dns", "server": "resolvable" },
                { "pool": "dns", "server": "missing" }
            ],
            "expectedActiveRevision": active_revision,
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(refreshed.status, 207, "{}", refreshed.text());
    let body = refreshed.json();
    assert_eq!(body["atomic"], false);
    assert_eq!(body["servers"].as_array().unwrap().len(), 2);
    assert_eq!(body["servers"][0]["outcome"], "refreshed");
    assert_eq!(body["servers"][1]["outcome"], "failed");
    assert_eq!(body["servers"][1]["error"]["code"], "dns_refresh_failed");

    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authenticated_local_shutdown_uses_the_graceful_process_channel() {
    let management_address = reserve_tcp_address();
    let config = management_config(management_address, None);
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let mut management = TcpStream::connect(management_address)
        .await
        .expect("persistent management connection");
    let active_revision = persistent_request(
        &mut management,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    let config_snapshot = persistent_request(
        &mut management,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    let disk_revision = config_snapshot["diskRevision"]
        .as_str()
        .expect("disk revision")
        .to_owned();
    let disk_before_shutdown = fs::read(&server.config_path).expect("canonical config");
    let mut changed_config = config.clone();
    changed_config.max_connections = Some(777);
    let config_body = serde_json::to_vec(&json!({ "config": changed_config })).unwrap();
    let mutation_body = serde_json::to_vec(&json!({
        "expectedActiveRevision": active_revision,
    }))
    .unwrap();

    let get = http_request(
        management_address,
        "GET",
        "/api/v1/process/shutdown",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(get.status, 405);
    let unauthorized = http_request(
        management_address,
        "POST",
        "/api/v1/process/shutdown",
        &[("Content-Type", "application/json")],
        &mutation_body,
    )
    .await;
    assert_eq!(unauthorized.status, 401);
    let drain = http_request(
        management_address,
        "POST",
        "/api/v1/process/drain",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &mutation_body,
    )
    .await;
    assert_eq!(drain.status, 202);
    let ready = http_request(management_address, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["ready"], false);
    let shutdown = persistent_request(
        &mut management,
        "POST",
        "/api/v1/process/shutdown",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &mutation_body,
    )
    .await;
    assert_eq!(shutdown.status, 202);
    assert_eq!(shutdown.json()["outcome"], "shutdown_requested");

    let validated = persistent_request(
        &mut management,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &config_body,
    )
    .await;
    assert_eq!(validated.status, 200, "{}", validated.text());
    let rejected = persistent_request(
        &mut management,
        "PUT",
        "/api/v1/config",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
            ("If-Config-Revision", &disk_revision),
        ],
        &config_body,
    )
    .await;
    assert_eq!(rejected.status, 409, "{}", rejected.text());
    assert_eq!(rejected.json()["error"]["code"], "mutation_in_progress");
    assert_eq!(
        fs::read(&server.config_path).expect("canonical config after rejected PUT"),
        disk_before_shutdown
    );

    drop(management);
    tokio::task::spawn_blocking(move || server.wait_for_exit())
        .await
        .expect("wait task");
}

#[test]
fn built_process_rejects_invalid_token_config_and_recording_roots() {
    let token_case = TempDir::new().expect("invalid token case");
    let token_config_path = token_case.path().join("oxiroute.lua");
    let token_config = management_config(reserve_tcp_address(), None);
    write_config(&token_config_path, &token_config);
    let token_path = write_token(token_case.path(), TOKEN, 0o644);
    let token_failure = run_to_failure(&token_config_path, Some(&token_path));
    assert!(!token_failure.status.success());
    let token_output = output_text(&token_failure);
    assert!(
        token_output.contains("candidate management token preparation failed"),
        "unexpected token failure: {token_output}"
    );
    assert!(!token_output.contains(TOKEN));
    assert!(!token_output.contains(&token_path.display().to_string()));

    let config_case = TempDir::new().expect("invalid config case");
    let invalid_config_path = config_case.path().join("invalid.lua");
    fs::write(&invalid_config_path, "return {").unwrap();
    let config_failure = run_to_failure(&invalid_config_path, None);
    assert!(!config_failure.status.success());
    let config_output = output_text(&config_failure);
    assert!(config_output.contains("canonical configuration was rejected"));

    let recording_case = TempDir::new().expect("invalid recording case");
    let recording_config_path = recording_case.path().join("oxiroute.lua");
    let secret_root = recording_case.path().join("missing-secret-recording-root");
    let recording_config = recording_candidate(&empty_config(), &secret_root);
    write_config(&recording_config_path, &recording_config);
    let recording_failure = run_to_failure(&recording_config_path, None);
    assert!(!recording_failure.status.success());
    let recording_output = output_text(&recording_failure);
    assert!(recording_output.contains("candidate runtime preparation failed"));
    assert!(!recording_output.contains(&secret_root.display().to_string()));
}

#[test]
fn built_process_fails_before_runtime_when_a_tcp_listener_cannot_bind() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let address = occupied.local_addr().expect("occupied address");
    let directory = TempDir::new().expect("bind failure case");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(&config_path, &rtmp_listener_config(socket_bind(address)));

    let failure = run_to_failure(&config_path, None);

    assert!(!failure.status.success());
    let output = output_text(&failure);
    assert!(
        output.contains("listener `live` could not bind socket"),
        "unexpected listener failure: {output}"
    );
}

#[cfg(unix)]
#[test]
fn built_process_does_not_unlink_an_existing_unix_listener_path() {
    let directory = TempDir::new().expect("Unix bind failure case");
    let path = directory.path().join("listener.sock");
    fs::write(&path, b"operator-owned").expect("existing Unix path");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(
        &config_path,
        &rtmp_listener_config(ListenerBind::Unix {
            path: path.clone(),
            mode: None,
        }),
    );

    let failure = run_to_failure(&config_path, None);

    assert!(!failure.status.success());
    assert_eq!(
        fs::read(path).expect("existing path retained"),
        b"operator-owned"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn built_process_activates_a_real_unix_listener() {
    let directory = TempDir::new().expect("Unix listener case");
    let socket_path = directory.path().join("listener.sock");
    let config = rtmp_listener_config(ListenerBind::Unix {
        path: socket_path.clone(),
        mode: None,
    });
    let mut server = ServerProcess::start(&config, None);

    server.wait_for_unix(&socket_path).await;

    assert!(socket_path.exists());
    server.shutdown();
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one process test covers validation, save, active preservation, and restart application"
)]
async fn unix_listener_mode_change_is_saved_as_restart_required_without_mutating_active_socket() {
    let directory = TempDir::new().expect("Unix mode reload case");
    let socket_path = directory.path().join("listener.sock");
    let management_address = reserve_tcp_address();
    let mut active = rtmp_listener_config(ListenerBind::Unix {
        path: socket_path.clone(),
        mode: Some(0o600),
    });
    active.management = Some(Management {
        bind: management_address,
        ui_dir: None,
    });
    let mut server = ServerProcess::start(&active, Some(TOKEN));
    server.wait_for_unix(&socket_path).await;
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let snapshot = http_request(
        management_address,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    let active_revision = snapshot["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();

    let mut candidate = active.clone();
    candidate.listeners[0].bind = ListenerBind::Unix {
        path: socket_path.clone(),
        mode: Some(0o660),
    };
    candidate.listeners[0].max_connections = Some(7);
    let body = serde_json::to_vec(&json!({ "config": candidate.clone() })).unwrap();
    let validated = http_request(
        management_address,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(validated.status, 200, "{}", validated.text());
    let validated = validated.json();
    assert_eq!(validated["restartRequired"], true);
    assert!(
        validated["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| diagnostics
                .iter()
                .any(|diagnostic| diagnostic["code"] == "I_RESTART_REQUIRED"))
    );
    assert_eq!(
        fs::metadata(&socket_path)
            .expect("validated active Unix socket")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let saved = http_request(
        management_address,
        "PUT",
        "/api/v1/config",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
            (
                "If-Config-Revision",
                snapshot["diskRevision"].as_str().expect("disk revision"),
            ),
        ],
        &body,
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.text());
    let saved = saved.json();
    assert_eq!(saved["outcome"], "saved_restart_required");
    assert_eq!(saved["activationState"], "restart_required");
    assert_eq!(saved["restartRequired"], true);
    assert_eq!(saved["activeRevision"], active_revision);
    assert!(saved["diagnostics"].as_array().is_some_and(|diagnostics| {
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "I_RESTART_REQUIRED")
    }));
    assert_eq!(
        fs::metadata(&socket_path)
            .expect("active Unix socket")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = http_request(
        management_address,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert_eq!(status["generation"]["activeRevision"], active_revision);
    assert_eq!(status["generation"]["quarantinedRevision"], Value::Null);
    assert_eq!(status["generation"]["lastFailure"], Value::Null);
    assert_eq!(status["generation"]["degraded"], false);
    let ready = http_request(management_address, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200, "{}", ready.text());
    server.shutdown();

    let mut restarted = ServerProcess::start(&candidate, Some(TOKEN));
    restarted.wait_for_unix(&socket_path).await;
    assert_eq!(
        fs::metadata(&socket_path)
            .expect("restarted Unix socket")
            .permissions()
            .mode()
            & 0o777,
        0o660
    );
    restarted.shutdown();
}

fn management_config(
    management_address: std::net::SocketAddr,
    ui_dir: Option<std::path::PathBuf>,
) -> Config {
    Config {
        management: Some(Management {
            bind: management_address,
            ui_dir,
        }),
        stats: None,
        ..empty_config()
    }
}

fn recording_candidate(active: &Config, root: &Path) -> Config {
    let mut candidate = active.clone();
    candidate.listeners.push(Listener {
        name: "recording-wire".into(),
        bind: socket_bind(reserve_tcp_address()),
        protocol: Protocol::Rtmp,
        service: Some("recording-wire".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    candidate.rtmp_services.push(RtmpService {
        name: "recording-wire".into(),
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
            idle_streams: false,
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
            recorders: vec![rtmp_recorder_with_queue_bytes(
                "archive",
                RtmpRecorderStart::Continuous,
                root,
                1024 * 1024,
            )],
        }],
    });
    candidate
}

fn rtmp_listener_config(bind: ListenerBind) -> Config {
    Config {
        listeners: vec![Listener {
            name: "live".into(),
            bind,
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
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
                idle_streams: false,
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
        }],
        ..empty_config()
    }
}

fn built_asset_paths(index: &[u8]) -> Vec<String> {
    let index = std::str::from_utf8(index).expect("built index UTF-8");
    let mut paths = Vec::new();
    for marker in ["src=\"", "href=\""] {
        let mut remainder = index;
        while let Some(start) = remainder.find(marker) {
            remainder = &remainder[start + marker.len()..];
            let end = remainder.find('"').expect("asset attribute terminator");
            let path = &remainder[..end];
            if path.starts_with("/assets/") {
                paths.push(path.to_owned());
            }
            remainder = &remainder[end + 1..];
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

async fn persistent_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> http_support::HttpResponse {
    use std::fmt::Write as _;

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n");
    for (name, value) in headers {
        writeln!(request, "{name}: {value}\r").expect("persistent request header");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT") {
        writeln!(request, "Content-Length: {}\r", body.len()).expect("persistent content length");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("persistent request head");
    stream
        .write_all(body)
        .await
        .expect("persistent request body");

    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    let (header_end, body_length) = loop {
        if let Some(header_end) = response.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&response[..header_end]).expect("response headers");
            let body_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (header_end + 4, body_length);
        }
        stream
            .read_exact(&mut byte)
            .await
            .expect("persistent response head");
        response.push(byte[0]);
    };
    while response.len() < header_end + body_length {
        stream
            .read_exact(&mut byte)
            .await
            .expect("persistent response body");
        response.push(byte[0]);
    }
    http_support::HttpResponse::parse(response)
}
