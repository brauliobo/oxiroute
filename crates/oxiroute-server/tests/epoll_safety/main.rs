#![cfg(target_os = "linux")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! What the epoll data plane must hold on to in production: the real binary in front of raw loopback origins, each
//! scenario on the epoll plane and, where the tokio plane has the same contract, on the tokio plane too.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/h1_wire.rs"]
mod h1_wire;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/process.rs"]
mod process_support;

use std::{
    future::Future,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use http_support::http_request;
use oxiroute_config::{
    AccessLogPolicy, ConfigDraft, DnsResolutionPolicy, HttpDataPlane, Management,
    UpstreamAlgorithm, UpstreamServer,
};
use process_support::{ServerProcess, reserve_tcp_address, write_config};
use rustix::{
    net::sockopt::set_socket_linger,
    process::{Pid, Resource, Rlimit, getrlimit, prlimit, setrlimit},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
    time::{sleep, timeout},
};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const CONFIG_WATCHER_THREAD: &str = "oxiroute-config";
const SERVED_ON_EPOLL: &str = "served by the epoll data plane";
const STEP: Duration = Duration::from_secs(10);
const TOKEN: &str = "5b1f0c6e8a3d4f27b9c0e1d2a3f4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2";

macro_rules! on_every_plane {
    ($scenario:ident) => {
        mod $scenario {
            #[tokio::test]
            async fn epoll() {
                super::$scenario(super::HttpDataPlane::Epoll).await;
            }

            #[tokio::test]
            async fn io_uring() {
                if oxiroute_epoll_h1::io_uring_support().is_ok() {
                    super::$scenario(super::HttpDataPlane::IoUring).await;
                }
            }

            #[tokio::test]
            async fn tokio() {
                super::$scenario(super::HttpDataPlane::Tokio).await;
            }
        }
    };
}

struct Scripted {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl Scripted {
    async fn start<F, Fut>(script: F) -> Self
    where
        F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let script = Arc::new(script);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                let script = Arc::clone(&script);
                tokio::spawn(async move { script(stream).await });
            }
        });
        Self { address, task }
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Proxy {
    address: SocketAddr,
    management: SocketAddr,
    config: ConfigDraft,
    server: ServerProcess,
}

impl Proxy {
    async fn start(
        origin: SocketAddr,
        plane: HttpDataPlane,
        edit: impl FnOnce(&mut ConfigDraft),
    ) -> Self {
        let (address, management) = (reserve_tcp_address(), reserve_tcp_address());
        let mut config = h1_wire::config(address, origin);
        config.management = Some(Management {
            bind: management,
            ui_dir: None,
            token_file: None,
        });
        config.log_filter = Some("oxiroute::epoll=debug".into());
        config.http_data_plane = Some(plane);
        config.http_services[0].upstream_io_timeout_ms = 30_000;
        edit(&mut config);
        let validated = config
            .clone()
            .validate()
            .expect("valid epoll safety config");
        let mut server = ServerProcess::start_with_env(&validated, Some(TOKEN), &[]);
        server.wait_for_tcp(address).await;
        server.wait_for_tcp(management).await;
        server.wait_for_thread(CONFIG_WATCHER_THREAD).await;
        Self {
            address,
            management,
            config,
            server,
        }
    }

    async fn managed(&self, path: &str) -> Value {
        let authorization = format!("Bearer {TOKEN}");
        let headers = [("Authorization", authorization.as_str())];
        http_request(self.management, "GET", path, &headers, &[])
            .await
            .json()
    }

    async fn revision(&self) -> String {
        self.managed("/api/v1/generations").await["generation"]["activeRevision"]
            .as_str()
            .expect("active revision")
            .to_owned()
    }

    async fn post(&self, path: &str, mut body: Value) -> Value {
        body["expectedActiveRevision"] = json!(self.revision().await);
        let authorization = format!("Bearer {TOKEN}");
        let response = http_request(
            self.management,
            "POST",
            path,
            &[
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
            ],
            &serde_json::to_vec(&body).expect("body"),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.text());
        response.json()
    }

    async fn evacuate(&self, force: bool) -> Value {
        let targets = [json!({ "pool": "origin", "server": "origin" })];
        let body = json!({ "targets": targets, "force": force });
        self.post("/api/v1/servers/evacuate", body).await
    }

    async fn evacuate_pool(&self, force: bool) -> Value {
        let body = json!({ "pools": ["origin"], "force": force });
        self.post("/api/v1/pools/evacuate", body).await
    }

    async fn reload(&mut self) {
        let before = self.revision().await;
        self.config.max_connections = Some(self.config.max_connections.unwrap_or(1_000) + 1);
        let validated = self.config.clone().validate().expect("valid reload config");
        write_config(&self.server.config_path, &validated);
        timeout(STEP, async {
            while self.revision().await == before {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the reloaded generation activated");
    }

    async fn connect(&mut self) -> TcpStream {
        match TcpStream::connect(self.address).await {
            Ok(stream) => stream,
            Err(error) => panic!(
                "connect {}: {error}; {}",
                self.address,
                self.server.report()
            ),
        }
    }

    async fn descriptors(&self) -> Option<u64> {
        self.managed("/api/v1/monitoring").await["process"]["openFileDescriptors"].as_u64()
    }

    fn set_descriptor_limit(&self, current: Option<u64>) {
        let pid = Pid::from_raw(i32::try_from(self.server.pid()).expect("a process id"));
        let maximum = getrlimit(Resource::Nofile).maximum;
        let new = Rlimit { current, maximum };
        prlimit(pid, Resource::Nofile, new).expect("limit the descriptors of the server");
    }

    fn limit_descriptors(&self, limit: u64) {
        self.set_descriptor_limit(Some(limit));
    }

    fn lift_descriptor_limit(&self) {
        self.set_descriptor_limit(getrlimit(Resource::Nofile).maximum);
    }

    fn cpu_ticks(&self) -> u64 {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.server.pid()));
        let stat = stat.expect("the statistics of the server");
        let fields = stat.rsplit_once(')').expect("a process name").1;
        let mut times = fields.split_whitespace().skip(11).take(2);
        times
            .by_ref()
            .map(|ticks| ticks.parse::<u64>().expect("ticks"))
            .sum()
    }

    fn engine_threads(&self) -> usize {
        let names = self.server.thread_names();
        names.iter().filter(|name| name.contains("-h1-")).count()
    }

    fn served_on_epoll(self) -> usize {
        self.server
            .shutdown_output()
            .lines()
            .filter(|line| line.contains(SERVED_ON_EPOLL))
            .count()
    }
}

fn expected_on_epoll(plane: HttpDataPlane, requests: usize) -> usize {
    if plane == HttpDataPlane::Tokio {
        0
    } else {
        requests
    }
}

async fn read_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk).await {
            Ok(count) if count > 0 => head.extend_from_slice(&chunk[..count]),
            _ => break,
        }
    }
    head
}

async fn read_until(stream: &mut TcpStream, needle: &str) -> String {
    let mut received = Vec::new();
    while !String::from_utf8_lossy(&received).contains(needle) {
        let mut chunk = [0_u8; 4096];
        let count = timeout(STEP, stream.read(&mut chunk))
            .await
            .expect("the proxy answered in time")
            .expect("read");
        assert_ne!(count, 0, "closed before `{needle}`");
        received.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8_lossy(&received).into_owned()
}

async fn read_to_end(stream: &mut TcpStream) -> String {
    read_to_end_within(stream, STEP).await
}

async fn read_to_end_within(stream: &mut TcpStream, limit: Duration) -> String {
    let mut rest = Vec::new();
    let _ = timeout(limit, stream.read_to_end(&mut rest))
        .await
        .expect("the proxy ended the connection in time");
    String::from_utf8_lossy(&rest).into_owned()
}

fn reset(stream: TcpStream) {
    let stream = stream.into_std().expect("std stream");
    set_socket_linger(&stream, Some(Duration::ZERO)).expect("zero linger");
    drop(stream);
}

async fn a_body_ended_by_an_origin_reset_is_not_completed(plane: HttpDataPlane) {
    let reset_now = Arc::new(Notify::new());
    let signal = Arc::clone(&reset_now);
    let origin = Scripted::start(move |mut stream| {
        let signal = Arc::clone(&signal);
        async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.0 200 OK\r\n\r\npart-one;")
                .await
                .expect("origin write");
            signal.notified().await;
            reset(stream);
        }
    })
    .await;
    let mut proxy = Proxy::start(origin.address, plane, |_| {}).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    let mut received = read_until(&mut client, "part-one;").await;
    reset_now.notify_one();
    received.push_str(&read_to_end(&mut client).await);
    assert!(
        !received.ends_with("0\r\n\r\n"),
        "a reset origin completed the body: {received:?}"
    );
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

on_every_plane!(a_body_ended_by_an_origin_reset_is_not_completed);

async fn a_response_in_flight_survives_the_replacement_of_its_generation(plane: HttpDataPlane) {
    const SECONDS: usize = 36;
    let origin = Scripted::start(|mut stream| async move {
        read_head(&mut stream).await;
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {SECONDS}\r\n\r\n");
        stream
            .write_all(head.as_bytes())
            .await
            .expect("origin head");
        for _ in 0..SECONDS {
            if stream.write_all(b"y").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await;
    let mut proxy = Proxy::start(origin.address, plane, |config| {
        config.http_services[0].upstream_io_timeout_ms = 120_000;
    })
    .await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    read_until(&mut client, "\r\n\r\ny").await;
    proxy.reload().await;
    let rest = read_to_end_within(&mut client, Duration::from_mins(1)).await;
    assert_eq!(rest.len(), SECONDS - 1, "the response was cut: {rest:?}");
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

on_every_plane!(a_response_in_flight_survives_the_replacement_of_its_generation);

async fn a_partial_request_head_is_closed_at_the_default_read_limit(plane: HttpDataPlane) {
    let origin = Scripted::start(|_| async {}).await;
    let mut proxy = Proxy::start(origin.address, plane, |_| {}).await;
    let mut client = proxy.connect().await;
    client
        .write_all(b"GET /r HTTP/1.1\r\nHost: example.test\r\nX-A:")
        .await
        .unwrap();
    let started = Instant::now();
    let _ = timeout(Duration::from_secs(90), client.read_to_end(&mut Vec::new()))
        .await
        .expect("a partial head stayed open past 90 seconds");
    let open = started.elapsed();
    assert!(
        (Duration::from_secs(55)..Duration::from_secs(75)).contains(&open),
        "a partial head was closed after {open:?}"
    );
}

on_every_plane!(a_partial_request_head_is_closed_at_the_default_read_limit);

fn raise_descriptor_limit(needed: u64) {
    let Rlimit { current, maximum } = getrlimit(Resource::Nofile);
    assert!(
        maximum.is_none_or(|maximum| maximum >= needed),
        "the test needs a hard descriptor limit of {needed}"
    );
    if current.is_some_and(|current| current < needed) {
        setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(needed),
                maximum,
            },
        )
        .expect("raise the soft descriptor limit");
    }
}

async fn answer_one_request(client: &mut TcpStream) {
    client.write_all(GET).await.unwrap();
    read_until(client, "ok").await;
}

const CLIENTS: usize = 17_000;
const CLIENTS_PER_BATCH: usize = 50;
const DESCRIPTORS_FOR_IDLE_CLIENTS: u64 = CLIENTS as u64 + 1_000;

async fn served_clients(address: SocketAddr, count: usize) -> Vec<TcpStream> {
    join_all((0..count).map(|_| async move {
        let mut client = TcpStream::connect(address).await.expect("connect");
        answer_one_request(&mut client).await;
        client
    }))
    .await
}

async fn more_idle_connections_than_a_thread_cap_are_all_served(plane: HttpDataPlane) {
    raise_descriptor_limit(DESCRIPTORS_FOR_IDLE_CLIENTS);
    let origin = Scripted::start(|mut stream| async move {
        while !read_head(&mut stream).await.is_empty() {
            stream.write_all(OK).await.expect("origin answer");
        }
    })
    .await;
    let proxy = Proxy::start(origin.address, plane, |config| config.log_filter = None).await;
    let mut clients = Vec::with_capacity(CLIENTS);
    for _ in 0..CLIENTS / CLIENTS_PER_BATCH {
        clients.extend(served_clients(proxy.address, CLIENTS_PER_BATCH).await);
    }
    answer_one_request(&mut clients[0]).await;
    answer_one_request(&mut clients[CLIENTS - 1]).await;
    assert_eq!(proxy.engine_threads(), expected_on_epoll(plane, 1));
}

on_every_plane!(more_idle_connections_than_a_thread_cap_are_all_served);

const FROM_A: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nfrom-a";
const FROM_B: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nfrom-b";
const STARTED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nfrom";

#[derive(Clone, Default)]
struct Held {
    arrived: Arc<Notify>,
    dropped: Arc<Notify>,
    release: Arc<Notify>,
}

impl Held {
    async fn origin(&self, first: &'static [u8], rest: &'static [u8]) -> Scripted {
        let held = self.clone();
        Scripted::start(move |mut stream| {
            let held = held.clone();
            async move {
                read_head(&mut stream).await;
                stream.write_all(first).await.expect("origin first bytes");
                held.arrived.notify_one();
                let mut probe = [0_u8; 1];
                tokio::select! {
                    () = held.release.notified() => {
                        stream.write_all(rest).await.expect("origin rest");
                    }
                    _ = stream.read(&mut probe) => held.dropped.notify_one(),
                }
            }
        })
        .await
    }

    async fn arrived(&self) {
        timeout(STEP, self.arrived.notified())
            .await
            .expect("the request reached the held origin");
    }

    async fn dropped(&self) {
        timeout(STEP, self.dropped.notified())
            .await
            .expect("the proxy closed the held origin connection");
    }
}

async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = read_head(stream).await;
    let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    let length = text
        .split("content-length:")
        .nth(1)
        .and_then(|rest| rest.split("\r\n").next())
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let head_end = text.find("\r\n\r\n").map_or(bytes.len(), |end| end + 4);
    while bytes.len() < head_end + length {
        let mut chunk = [0_u8; 8192];
        match stream.read(&mut chunk).await {
            Ok(count) if count > 0 => bytes.extend_from_slice(&chunk[..count]),
            _ => break,
        }
    }
    bytes
}

async fn replying_origin(reply: &'static [u8]) -> Scripted {
    Scripted::start(move |mut stream| async move {
        while !read_request(&mut stream).await.is_empty() {
            stream.write_all(reply).await.expect("origin answer");
        }
    })
    .await
}

fn with_second_server(config: &mut ConfigDraft, second: SocketAddr, algorithm: UpstreamAlgorithm) {
    config.upstream_pools[0].algorithm = algorithm;
    config.upstream_pools[0].servers.push(UpstreamServer {
        name: "origin-b".into(),
        endpoint: config_support::socket_endpoint(second),
        max_connections: None,
        dns_resolution: DnsResolutionPolicy::OnConnect,
    });
}

async fn proxy_over_two_origins(
    plane: HttpDataPlane,
    first: &Scripted,
    second: &Scripted,
) -> Proxy {
    let second = second.address;
    Proxy::start(first.address, plane, move |config| {
        with_second_server(config, second, UpstreamAlgorithm::First);
    })
    .await
}

fn evacuation_report(forced: bool, moved: u64, cut: u64, left: &Value) -> Value {
    json!({
        "outcome": "evacuated",
        "force": forced,
        "cancelledAndRedispatched": moved,
        "forced": cut,
        "leftRunning": left,
    })
}

async fn a_request_waiting_on_an_evacuated_server_is_sent_to_another(plane: HttpDataPlane) {
    let held = Held::default();
    let (first, second) = (
        held.origin(b"", FROM_A).await,
        replying_origin(FROM_B).await,
    );
    let mut proxy = proxy_over_two_origins(plane, &first, &second).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    held.arrived().await;
    let report = proxy.evacuate(false).await;
    assert_eq!(report, evacuation_report(false, 1, 0, &json!({})));
    held.dropped().await;
    let answer = read_until(&mut client, "from-").await;
    assert!(answer.ends_with("from-b"), "{answer}");
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

async fn forcing_an_evacuation_cuts_a_request_waiting_on_it(plane: HttpDataPlane) {
    let held = Held::default();
    let (first, second) = (
        held.origin(b"", FROM_A).await,
        replying_origin(FROM_B).await,
    );
    let mut proxy = proxy_over_two_origins(plane, &first, &second).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    held.arrived().await;
    let report = proxy.evacuate_pool(true).await;
    assert_eq!(report, evacuation_report(true, 0, 1, &json!({})));
    held.dropped().await;
    let answer = read_to_end(&mut client).await;
    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert!(
        answer
            .to_ascii_lowercase()
            .contains("x-oxiroute-reason: evacuated"),
        "{answer}"
    );
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

async fn a_response_that_started_is_left_running_by_an_evacuation(plane: HttpDataPlane) {
    let held = Held::default();
    let (first, second) = (
        held.origin(STARTED, b"-a").await,
        replying_origin(FROM_B).await,
    );
    let mut proxy = proxy_over_two_origins(plane, &first, &second).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    read_until(&mut client, "from").await;
    held.arrived().await;
    let report = proxy.evacuate(false).await;
    let left = json!({ "response_started": 1 });
    assert_eq!(report, evacuation_report(false, 0, 0, &left));
    held.release.notify_one();
    let rest = read_until(&mut client, "-a").await;
    assert!(rest.ends_with("-a"), "{rest}");
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

async fn forcing_an_evacuation_resets_a_response_that_started(plane: HttpDataPlane) {
    let held = Held::default();
    let (first, second) = (
        held.origin(STARTED, b"-a").await,
        replying_origin(FROM_B).await,
    );
    let mut proxy = proxy_over_two_origins(plane, &first, &second).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    read_until(&mut client, "from").await;
    held.arrived().await;
    let report = proxy.evacuate(true).await;
    assert_eq!(report, evacuation_report(true, 0, 1, &json!({})));
    held.dropped().await;
    let rest = read_to_end(&mut client).await;
    assert!(!rest.contains("-a"), "{rest}");
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

on_every_plane!(a_request_waiting_on_an_evacuated_server_is_sent_to_another);
on_every_plane!(forcing_an_evacuation_cuts_a_request_waiting_on_it);
on_every_plane!(a_response_that_started_is_left_running_by_an_evacuation);
#[tokio::test]
async fn forcing_an_evacuation_resets_a_response_that_started_at_once_on_epoll() {
    forcing_an_evacuation_resets_a_response_that_started(HttpDataPlane::Epoll).await;
}

#[tokio::test]
async fn a_response_with_a_malformed_first_chunk_is_answered_with_a_502() {
    let plane = HttpDataPlane::Epoll;
    let origin =
        replying_origin(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nbroken\r\n")
            .await;
    let mut proxy = Proxy::start(origin.address, plane, |_| {}).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    let answer = read_to_end(&mut client).await;
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer:?}");
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

async fn access_record(path: &std::path::Path) -> Value {
    timeout(STEP, async {
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if let Some(line) = text.lines().next() {
                return serde_json::from_str(line).expect("a JSON access line");
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the access record was written")
}

async fn the_connect_time_is_logged_when_the_origin_closes_before_answering(plane: HttpDataPlane) {
    let origin = Scripted::start(|mut stream| async move {
        read_head(&mut stream).await;
    })
    .await;
    let directory = tempfile::tempdir().expect("log directory");
    let log = directory.path().join("access.log");
    let path = log.clone();
    let mut proxy = Proxy::start(origin.address, plane, move |config| {
        config.http_services[0].access_log = Some(AccessLogPolicy::File { path });
    })
    .await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    let answer = read_to_end(&mut client).await;
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer:?}");
    let record = access_record(&log).await;
    assert!(!record["connectMs"].is_null(), "{record}");
}

on_every_plane!(the_connect_time_is_logged_when_the_origin_closes_before_answering);

async fn descriptors_exhausted_at_accept_neither_spin_the_listener_nor_stop_it(
    plane: HttpDataPlane,
) {
    let origin = replying_origin(OK).await;
    let mut proxy = Proxy::start(origin.address, plane, |config| {
        config.log_filter = None;
    })
    .await;
    let idle = proxy
        .descriptors()
        .await
        .expect("the descriptors of the server");
    proxy.limit_descriptors(idle + 4);
    let mut held = Vec::new();
    for _ in 0..60 {
        held.push(proxy.connect().await);
    }
    let before = proxy.cpu_ticks();
    sleep(Duration::from_millis(1500)).await;
    let spent = proxy.cpu_ticks() - before;
    assert!(
        spent < 45,
        "the server spent {spent} ticks of 150 while it could not accept"
    );
    proxy.lift_descriptor_limit();
    answer_one_request(&mut proxy.connect().await).await;
    drop(held);
}

#[tokio::test]
async fn descriptors_exhausted_at_accept_neither_spin_nor_stop_the_epoll_listener() {
    descriptors_exhausted_at_accept_neither_spin_the_listener_nor_stop_it(HttpDataPlane::Epoll)
        .await;
}

#[tokio::test]
async fn descriptors_exhausted_at_accept_neither_spin_nor_stop_the_io_uring_listener() {
    if oxiroute_epoll_h1::io_uring_support().is_err() {
        return;
    }
    descriptors_exhausted_at_accept_neither_spin_the_listener_nor_stop_it(HttpDataPlane::IoUring)
        .await;
}
