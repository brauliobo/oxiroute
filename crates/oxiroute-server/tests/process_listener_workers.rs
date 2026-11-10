#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

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

const AGED: Option<ConnectionScheduling> = Some(ConnectionScheduling::AgedPerWorker);
const REQUEST: &[u8] = b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

fn config(
    address: SocketAddr,
    workers: Option<u64>,
    scheduling: Option<ConnectionScheduling>,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: workers,
        connection_scheduling: scheduling,
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

/// Process and thread ids of the threads named after the listener's service, in the process and everything it
/// supervises.
fn service_tasks(root: u32) -> Vec<(u32, u32)> {
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
                .map(move |task| (*pid, task))
        })
        .filter(|(_, task)| {
            fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim() == "OxiRoute web")
        })
        .filter_map(|(pid, task)| Some((pid, task.file_name().to_str()?.parse().ok()?)))
        .collect()
}

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

/// The inodes of the server-side sockets of the given client ports, from the kernel's TCP table.
fn connection_inodes(server: SocketAddr, client_ports: &[u16]) -> Vec<String> {
    fs::read_to_string("/proc/net/tcp")
        .expect("tcp table")
        .lines()
        .skip(1)
        .filter_map(|line| {
            let columns = line.split_whitespace().collect::<Vec<_>>();
            let port = |column: &str| u16::from_str_radix(column.split_once(':')?.1, 16).ok();
            let (local, remote) = (port(columns[1])?, port(columns[2])?);
            (local == server.port() && client_ports.contains(&remote))
                .then(|| columns[9].to_owned())
        })
        .collect()
}

/// For every epoll instance of the process, how many of the given socket inodes it watches. A runtime keeps a duplicate
/// descriptor of its epoll instance, so the descriptors with identical listings count once.
fn watched_per_epoll(pid: u32, inodes: &[String]) -> Vec<usize> {
    let target = |fd: &str| {
        fs::read_link(format!("/proc/{pid}/fd/{fd}")).map(|link| link.display().to_string())
    };
    let sockets = inodes
        .iter()
        .map(|inode| format!("socket:[{inode}]"))
        .collect::<Vec<_>>();
    let listings = fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("fd table")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|fd| target(fd).is_ok_and(|link| link == "anon_inode:[eventpoll]"))
        .map(|epoll| {
            fs::read_to_string(format!("/proc/{pid}/fdinfo/{epoll}")).expect("epoll fdinfo")
        })
        .collect::<BTreeSet<_>>();
    listings
        .iter()
        .map(|listing| {
            listing
                .lines()
                .filter_map(|line| line.strip_prefix("tfd:")?.split_whitespace().next())
                .filter(|fd| target(fd).is_ok_and(|link| sockets.contains(&link)))
                .count()
        })
        .collect()
}

const KEEP_ALIVE: &[u8] = b"GET /probe HTTP/1.1\r\nHost: localhost\r\n\r\n";

async fn open_keep_alive(address: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.expect("connect");
    stream.write_all(KEEP_ALIVE).await.expect("request");
    let mut response = Vec::new();
    while !response.ends_with(b"ok") {
        let mut chunk = [0_u8; 512];
        let read = stream.read(&mut chunk).await.expect("response");
        assert_ne!(read, 0, "closed early");
        response.extend_from_slice(&chunk[..read]);
    }
    stream
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
