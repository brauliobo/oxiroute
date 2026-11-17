use std::time::{Duration, Instant};

use oxiroute_config::{
    DnsResolutionPolicy, HttpVersionPolicy, Management, Stats, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpStream;

use crate::config_support::empty_config;
use crate::http_support::http_request;
use crate::process_support::{self, ServerProcess, reserve_tcp_address, write_config, write_token};
use crate::shared::{TOKEN, management_config, persistent_request};

#[tokio::test(flavor = "current_thread")]
async fn generation_status_remains_responsive_through_publication() {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        Some(TOKEN),
    );
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
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
    let status_reads = tokio::time::timeout(process_support::PROCESS_TIMEOUT, status_reader)
        .await
        .expect("generation reload timed out")
        .expect("status reader");
    assert!(status_reads > 1, "status was not sampled continuously");

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
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }];
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        Some(TOKEN),
    );
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
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
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
        if let Some(revision) = response.json()["generation"]["activeRevision"].as_str()
            && revision != original_revision
        {
            break revision.to_owned();
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
        token_file: None,
    });
    config.stats = Some(Stats {
        binds: vec![original_bind],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        Some(TOKEN),
    );
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
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
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
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
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
