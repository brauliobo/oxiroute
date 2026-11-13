#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Reloading a configuration again and again does not grow the process: the generations a reload replaces give their
//! memory back, so the anonymous memory after many reloads stays where it was after the first few. The proxy is the
//! real binary with a configuration of many routes, so a generation that is kept (or leaks part of itself) costs a
//! measurable part of the process each time.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, time::Duration};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy, HttpRoute,
    HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy, Listener, Management,
    Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use serde_json::Value;
use tokio::time::sleep;

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const ROUTES: usize = 400;
const SETTLED_RELOADS: usize = 3;
const RELOADS: usize = 12;
/// Bytes of anonymous memory the process may gain between the reloads that settle it and the last one (1.5 MiB measured).
const GROWTH_CEILING: usize = 3 << 20;

fn config(
    management: std::net::SocketAddr,
    listener: std::net::SocketAddr,
    revision: u64,
) -> ConfigDraft {
    let route = |index: usize| HttpRoute {
        host: None,
        path: HttpPathSelector::Exact {
            value: format!("/route-{index}"),
        },
        methods: Vec::new(),
        access_policy: None,
        policy: HttpRoutePolicy::default(),
        action: HttpRouteAction::Proxy {
            upstream_pool: "upstream".into(),
            policy: HttpProxyPolicy::default().into(),
        },
    };
    ConfigDraft {
        max_connections: Some(1_000 + revision),
        management: Some(Management {
            bind: management,
            ui_dir: None,
            token_file: None,
        }),
        listeners: vec![Listener {
            name: "web".into(),
            bind: socket_bind(listener),
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        upstream_pools: vec![UpstreamPool {
            name: "upstream".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint("127.0.0.1:9".parse().expect("address"))],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: Some(1_000),
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "web".into(),
            routes: (0..ROUTES).map(route).collect(),
            automatic_response_headers: true,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    }
}

fn anonymous_bytes(pid: u32) -> usize {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("process status");
    let kilobytes = status
        .lines()
        .find_map(|line| line.strip_prefix("RssAnon:"))
        .and_then(|rest| {
            rest.trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<usize>()
                .ok()
        })
        .expect("RssAnon of the process");
    kilobytes * 1024
}

async fn active_revision(management: std::net::SocketAddr) -> Value {
    let authorization = format!("Bearer {TOKEN}");
    http_request(
        management,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .clone()
}

#[tokio::test]
async fn reloading_many_times_does_not_grow_the_process() {
    let (management, listener) = (reserve_tcp_address(), reserve_tcp_address());
    let validated = config(management, listener, 0)
        .validate()
        .expect("valid config");
    let mut server = ServerProcess::start(&validated, Some(TOKEN));
    server.wait_for_tcp(management).await;
    server.wait_for_tcp(listener).await;

    let mut settled = 0;
    for revision in 1..=RELOADS {
        let before = active_revision(management).await;
        write_config(
            &server.config_path,
            &config(management, listener, revision as u64)
                .validate()
                .expect("valid config"),
        );
        for _ in 0..400 {
            if active_revision(management).await != before {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
        assert_ne!(
            active_revision(management).await,
            before,
            "reload {revision} did not activate"
        );
        // The generation that was replaced leaves on the supervisor's next tick.
        sleep(Duration::from_millis(500)).await;
        if revision == SETTLED_RELOADS {
            settled = anonymous_bytes(server.pid());
        }
    }
    let last = anonymous_bytes(server.pid());
    let growth = last.saturating_sub(settled);
    println!(
        "anonymous memory after reload {SETTLED_RELOADS}: {settled}, after reload {RELOADS}: {last}"
    );
    assert!(
        growth <= GROWTH_CEILING,
        "{} reloads after the process settled added {growth} bytes, ceiling {GROWTH_CEILING}",
        RELOADS - SETTLED_RELOADS
    );
    server.shutdown();
}
