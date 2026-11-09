#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, net::SocketAddr};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpPathSelector, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpService, Listener, Protocol,
};

use config_support::{empty_config, socket_bind};
use http_support::raw_http_request;
use process_support::{ServerProcess, reserve_tcp_address};

const REQUEST: &[u8] = b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

fn config(address: SocketAddr, workers: Option<u64>) -> ConfigDraft {
    ConfigDraft {
        listener_workers: workers,
        listeners: vec![Listener {
            name: "web".into(),
            bind: socket_bind(address),
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "web".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/probe".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::FixedResponse {
                    status: 200,
                    body: "ok".into(),
                    headers: Vec::new(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    }
}

/// Threads named after the listener's service in the process and everything it supervises.
fn service_threads(root: u32) -> usize {
    let parents = fs::read_dir("/proc")
        .expect("proc")
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_string_lossy().parse::<u32>().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let parent = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(1)?
                .parse::<u32>()
                .ok()?;
            Some((pid, parent))
        })
        .collect::<Vec<_>>();
    let mut tree = vec![root];
    let mut next = 0;
    while next < tree.len() {
        let parent = tree[next];
        tree.extend(
            parents
                .iter()
                .filter(|(_, candidate)| *candidate == parent)
                .map(|(pid, _)| *pid),
        );
        next += 1;
    }
    tree.iter()
        .flat_map(|pid| {
            fs::read_dir(format!("/proc/{pid}/task"))
                .into_iter()
                .flatten()
                .flatten()
        })
        .filter(|task| {
            fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim() == "OxiRoute web")
        })
        .count()
}

async fn serve_and_count(workers: Option<u64>) -> usize {
    let address = reserve_tcp_address();
    let mut server =
        ServerProcess::start(&config(address, workers).validate().expect("valid"), None);
    server.wait_for_tcp(address).await;
    let responses =
        futures_util::future::join_all((0..64).map(|_| raw_http_request(address, REQUEST))).await;
    assert!(responses.iter().all(|response| response.status == 200));
    let threads = service_threads(server.pid());
    server.shutdown();
    threads
}

#[tokio::test]
async fn a_listener_runs_the_configured_number_of_workers() {
    assert_eq!(serve_and_count(Some(3)).await, 3);
    assert_eq!(serve_and_count(Some(1)).await, 1);
}

#[tokio::test]
async fn a_listener_without_a_setting_runs_one_worker() {
    assert_eq!(serve_and_count(None).await, 1);
}
