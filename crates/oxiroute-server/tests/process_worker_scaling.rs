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

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use oxiroute_config::{ConfigDraft, WorkerScaling};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use process_support::{ServerProcess, reserve_tcp_address};
use workers_support::{
    AGED, KEEP_ALIVE, config, connection_inodes, open_keep_alive, service_tasks, watched_per_epoll,
};

const WORKERS: u64 = 4;
const CONNECTIONS: usize = 48;

fn dynamic(address: SocketAddr) -> ConfigDraft {
    ConfigDraft {
        worker_scaling: Some(WorkerScaling::Dynamic),
        scale_down_delay_ms: Some(200),
        ..config(address, Some(WORKERS), AGED)
    }
}

async fn ask(stream: &mut TcpStream) {
    stream.write_all(KEEP_ALIVE).await.expect("request");
    let mut response = Vec::new();
    while !response.ends_with(b"ok") {
        let mut chunk = [0_u8; 512];
        let read = stream.read(&mut chunk).await.expect("response");
        assert_ne!(read, 0, "closed early");
        response.extend_from_slice(&chunk[..read]);
    }
}

/// How many of the connections each worker's epoll instance watches, largest first.
fn watched(server: &ServerProcess, address: SocketAddr, clients: &[TcpStream]) -> Vec<usize> {
    let ports = clients
        .iter()
        .map(|client| client.local_addr().expect("port").port())
        .collect::<Vec<_>>();
    let (pid, _) = service_tasks(server.pid())[0];
    let inodes = connection_inodes(address, &ports);
    assert_eq!(inodes.len(), clients.len());
    let mut counts = watched_per_epoll(pid, &inodes);
    counts.sort_unstable_by(|a, b| b.cmp(a));
    counts
}

/// Light traffic on many keep-alive connections: the connections end up on one worker, every request is answered
/// on the way, and the connections keep working afterwards.
#[tokio::test]
async fn idle_keep_alive_connections_are_packed_onto_one_worker_and_keep_serving() {
    let address = reserve_tcp_address();
    let mut server = ServerProcess::start(&dynamic(address).validate().expect("valid"), None);
    server.wait_for_tcp(address).await;
    let mut clients = Vec::new();
    for _ in 0..CONNECTIONS {
        clients.push(open_keep_alive(address).await);
    }
    let spread = watched(&server, address, &clients);
    assert_eq!(spread.iter().sum::<usize>(), CONNECTIONS, "{spread:?}");
    assert!(
        spread[0] < CONNECTIONS,
        "connections start spread over the workers: {spread:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        tokio::time::sleep(Duration::from_millis(150)).await;
        for client in &mut clients {
            ask(client).await;
        }
        let packed = watched(&server, address, &clients);
        if packed[0] == CONNECTIONS {
            break;
        }
        assert!(Instant::now() < deadline, "never packed: {packed:?}");
    }
    for client in &mut clients {
        ask(client).await;
    }
    drop(clients);
    server.shutdown();
}

/// With the option absent connections stay where they were placed however long they idle.
#[tokio::test]
async fn static_scaling_keeps_the_connections_spread() {
    let address = reserve_tcp_address();
    let mut server = ServerProcess::start(
        &config(address, Some(WORKERS), AGED)
            .validate()
            .expect("valid"),
        None,
    );
    server.wait_for_tcp(address).await;
    let mut clients = Vec::new();
    for _ in 0..CONNECTIONS {
        clients.push(open_keep_alive(address).await);
    }
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        for client in &mut clients {
            ask(client).await;
        }
    }
    let counts = watched(&server, address, &clients);
    assert!(counts[0] < CONNECTIONS, "{counts:?}");
    drop(clients);
    server.shutdown();
}
