#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/workers.rs"]
mod workers_support;

use std::{collections::BTreeSet, fs, net::SocketAddr};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use oxiroute_config::{
    ConfigDraft, ConnectionScheduling, DownstreamTimeoutPolicy, HttpPathSelector, HttpRoute,
    HttpRouteAction, HttpRoutePolicy, HttpService, Listener, Protocol,
};

use config_support::{empty_config, socket_bind};
use http_support::raw_http_request;
use process_support::{ServerProcess, reserve_tcp_address};
use workers_support::{
    AGED, REQUEST, config, connection_inodes, open_keep_alive, service_tasks, watched_per_epoll,
};

fn service_threads(root: u32) -> usize {
    service_tasks(root).len()
}

async fn serve_and_count(workers: Option<u64>, scheduling: Option<ConnectionScheduling>) -> usize {
    let address = reserve_tcp_address();
    let config = config(address, workers, scheduling);
    let mut server = ServerProcess::start(&config.validate().expect("valid"), None);
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
    assert_eq!(serve_and_count(Some(3), None).await, 3);
    assert_eq!(serve_and_count(Some(1), None).await, 1);
}

#[tokio::test]
async fn a_listener_without_a_setting_runs_one_worker() {
    assert_eq!(serve_and_count(None, None).await, 1);
}

#[tokio::test]
async fn aged_per_worker_scheduling_runs_the_configured_number_of_workers_and_serves() {
    assert_eq!(serve_and_count(Some(3), AGED).await, 3);
    assert_eq!(serve_and_count(None, AGED).await, 1);
}

/// A connection must be watched by the epoll instance of the worker that serves it: a socket registered with another
/// worker's driver makes that worker wake the serving one for every readiness event.
#[tokio::test]
async fn workers_watch_their_own_connections() {
    let address = reserve_tcp_address();
    let mut server = ServerProcess::start(
        &config(address, Some(4), AGED).validate().expect("valid"),
        None,
    );
    server.wait_for_tcp(address).await;
    let mut clients = Vec::new();
    for _ in 0..48 {
        clients.push(open_keep_alive(address).await);
    }
    let ports = clients
        .iter()
        .map(|client| client.local_addr().expect("port").port())
        .collect::<Vec<_>>();
    let (pid, _) = service_tasks(server.pid())[0];
    let inodes = connection_inodes(address, &ports);
    assert_eq!(inodes.len(), 48);
    let mut watched = watched_per_epoll(pid, &inodes);
    watched.sort_unstable();
    assert_eq!(watched.iter().sum::<usize>(), 48, "{watched:?}");
    let busy = watched.iter().rev().take(4).copied().collect::<Vec<_>>();
    assert!(
        busy.iter().all(|count| (10..=14).contains(count)),
        "{watched:?}"
    );
    drop(clients);
    server.shutdown();
}
