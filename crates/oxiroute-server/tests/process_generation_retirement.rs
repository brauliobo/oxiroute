#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod wire_support;

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy, HttpRoute,
    HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy, L4Service, Listener,
    Management, Protocol, Retirement, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const WIRE_TIMEOUT: Duration = Duration::from_secs(10);
static PROCESS_TEST_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

async fn process_test_guard() -> OwnedSemaphorePermit {
    PROCESS_TEST_GATE
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("process test gate remains open")
}

struct Instance {
    server: ServerProcess,
    management: SocketAddr,
    listener: SocketAddr,
    config: ConfigDraft,
    upstream: JoinHandle<()>,
    release: Arc<Semaphore>,
    sequence: u64,
}

impl Instance {
    async fn start(retirement: Retirement) -> Self {
        Self::start_tcp(retirement, false).await
    }

    async fn start_tcp(retirement: Retirement, supervised: bool) -> Self {
        let management = reserve_tcp_address();
        let listener = reserve_tcp_address();
        let (upstream_address, upstream) = start_echo_upstream().await;
        let config = tcp_config(management, listener, upstream_address, retirement);
        let mut server = start_server(&config, supervised);
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(listener).await;
        Self {
            server,
            management,
            listener,
            config,
            upstream,
            release: Arc::new(Semaphore::new(0)),
            sequence: 0,
        }
    }

    async fn start_http(retirement: Retirement) -> Self {
        Self::start_http_mode(retirement, false).await
    }

    async fn start_http_mode(retirement: Retirement, supervised: bool) -> Self {
        let management = reserve_tcp_address();
        let listener = reserve_tcp_address();
        let release = Arc::new(Semaphore::new(0));
        let (upstream_address, upstream) = start_held_http_upstream(Arc::clone(&release)).await;
        let config = http_config(management, listener, upstream_address, retirement);
        let mut server = start_server(&config, supervised);
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(listener).await;
        Self {
            server,
            management,
            listener,
            config,
            upstream,
            release,
            sequence: 0,
        }
    }

    async fn get(&self, path: &str) -> Value {
        let authorization = format!("Bearer {TOKEN}");
        http_request(
            self.management,
            "GET",
            path,
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .json()
    }

    async fn generation(&self) -> Value {
        self.get("/api/v1/generations").await["generation"].clone()
    }

    async fn metrics(&self) -> String {
        let response = http_request(self.management, "GET", "/metrics", &[], &[]).await;
        String::from_utf8(response.body).expect("metrics UTF-8")
    }

    /// Rewrites the file with a different effective revision and returns the new revision.
    fn write_next_revision(&mut self) {
        self.sequence += 1;
        self.config.max_connections = Some(1_000 + self.sequence);
        if let Some(HttpRouteAction::FixedResponse { body, .. }) = self
            .config
            .http_services
            .first_mut()
            .map(|service| &mut service.routes[0].action)
        {
            *body = format!("revision-{}", self.sequence);
        }
        write_config(
            &self.server.config_path,
            &self.config.clone().validate().unwrap(),
        );
    }

    async fn apply(&mut self) -> Value {
        let before = self.generation().await["activeRevision"].clone();
        self.write_next_revision();
        wait_until("the new revision activates", || async {
            let generation = self.generation().await;
            (generation["activeRevision"] != before).then_some(generation)
        })
        .await
    }

    async fn event_outcomes(&self, name: &str) -> Vec<String> {
        self.get("/api/v2/events?after=0&limit=200").await["events"]
            .as_array()
            .expect("events")
            .iter()
            .filter(|event| event["event"] == name)
            .map(|event| event["outcome"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn stop(self) {
        self.upstream.abort();
        self.server.shutdown();
    }
}

fn start_server(config: &ConfigDraft, supervised: bool) -> ServerProcess {
    let validated = config.clone().validate().unwrap();
    if supervised {
        ServerProcess::start_supervised(&validated, TOKEN)
    } else {
        ServerProcess::start(&validated, Some(TOKEN))
    }
}

async fn wait_until<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WIRE_TIMEOUT;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(25)).await;
    }
}

async fn open_relay(instance: &Instance) -> TcpStream {
    let mut stream = TcpStream::connect(instance.listener)
        .await
        .expect("relay connection");
    assert_echo(&mut stream, b"hello").await;
    stream
}

#[tokio::test]
async fn retired_generations_stay_listed_while_their_relays_are_open() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start(Retirement::default()).await;
    let mut first_relay = open_relay(&instance).await;
    let first_revision = instance.generation().await["activeRevision"].clone();
    instance.apply().await;
    let second_relay = open_relay(&instance).await;
    let second_revision = instance.generation().await["activeRevision"].clone();
    let status = instance.apply().await;

    let retired = status["retiredGenerations"]
        .as_array()
        .expect("retired list");
    assert_eq!(retired.len(), 2, "{status}");
    assert_eq!(retired[0]["revision"], first_revision, "oldest first");
    assert_eq!(retired[1]["revision"], second_revision);
    for entry in retired {
        assert_eq!(entry["state"], "draining");
        assert_eq!(entry["inFlight"], 1);
        assert_eq!(entry["inFlightByKind"]["tcp"], 1);
        assert!(entry["deadlineRemainingSeconds"].as_u64().unwrap() > 10_000);
    }
    assert_eq!(status["maxRetiredGenerations"], 4);
    let metrics = instance.metrics().await;
    assert!(
        metrics.contains("oxiroute_generation_retired 2"),
        "{metrics}"
    );
    assert!(metrics.contains("oxiroute_generation_retired_age_seconds{revision="));
    assert!(metrics.contains("role=\"retired\"} 1"));

    assert_echo(&mut first_relay, b"still served").await;
    drop(first_relay);
    let status = wait_until("the first generation drains", || async {
        let status = instance.generation().await;
        (status["retiredGenerations"].as_array().unwrap().len() == 1).then_some(status)
    })
    .await;
    assert_eq!(status["retiredGenerations"][0]["revision"], second_revision);
    drop(second_relay);
    wait_until("the second generation drains", || async {
        let status = instance.generation().await;
        status["retiredGenerations"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    })
    .await;
    assert!(
        instance
            .metrics()
            .await
            .contains("oxiroute_generation_retired 0")
    );
    let outcomes = instance.event_outcomes("generation_retire").await;
    assert_eq!(
        outcomes.iter().filter(|o| *o == "retiring").count(),
        2,
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes.iter().filter(|o| *o == "drained").count(),
        2,
        "{outcomes:?}"
    );
    instance.stop();
}

#[tokio::test]
async fn a_full_retired_cap_defers_the_next_apply_until_a_slot_frees() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start(Retirement {
        drain_deadline_ms: None,
        max_retired_generations: 1,
    })
    .await;
    let relay = open_relay(&instance).await;
    instance.apply().await;
    let active = instance.generation().await["activeRevision"].clone();

    instance.write_next_revision();
    let status = wait_until("the apply is deferred", || async {
        let status = instance.generation().await;
        (status["pendingApply"]["reason"] == "retired_cap").then_some(status)
    })
    .await;
    assert_eq!(
        status["activeRevision"], active,
        "the old generation keeps serving"
    );
    assert_eq!(
        status["pendingApply"]["revision"],
        status["candidateRevision"]
    );
    assert_eq!(status["maxRetiredGenerations"], 1);
    assert!(
        instance
            .metrics()
            .await
            .contains("oxiroute_generation_retired_cap_deferrals_total 1")
    );
    sleep(Duration::from_millis(300)).await;
    assert_eq!(instance.generation().await["activeRevision"], active);

    drop(relay);
    let status = wait_until("the deferred apply activates", || async {
        let status = instance.generation().await;
        (status["activeRevision"] != active).then_some(status)
    })
    .await;
    assert!(status["pendingApply"].is_null(), "{status}");
    assert!(
        instance
            .event_outcomes("generation_retire")
            .await
            .contains(&"cap_reached".into())
    );
    instance.stop();
}

#[tokio::test]
async fn a_relay_that_outlives_the_drain_deadline_is_closed() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start(Retirement {
        drain_deadline_ms: Some(1_500),
        max_retired_generations: 4,
    })
    .await;
    let mut relay = open_relay(&instance).await;
    let applied = Instant::now();
    instance.apply().await;
    assert_echo(&mut relay, b"before the deadline").await;

    let mut buffer = [0_u8; 16];
    let closed = timeout(Duration::from_secs(8), relay.read(&mut buffer))
        .await
        .expect("the relay was never closed");
    assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");
    let elapsed = applied.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1_400),
        "closed too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "closed too late: {elapsed:?}"
    );

    let status = wait_until("the closed generation leaves the list", || async {
        let status = instance.generation().await;
        status["retiredGenerations"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(status)
    })
    .await;
    assert!(status["pendingApply"].is_null());
    let metrics = instance.metrics().await;
    assert!(
        metrics.contains("oxiroute_generation_forced_close_total{reason=\"drain_deadline\"} 1"),
        "{metrics}"
    );
    assert!(
        instance
            .event_outcomes("generation_retire")
            .await
            .contains(&"forced_close".into())
    );
    instance.stop();
}

fn tcp_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
    retirement: Retirement,
) -> ConfigDraft {
    ConfigDraft {
        retirement: Some(retirement),
        management: Some(Management {
            bind: management_address,
            ui_dir: None,
        }),
        listeners: vec![Listener {
            name: "tcp".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: "upstream".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(upstream_address)],
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
        l4_services: vec![L4Service {
            drain_deadline_ms: None,
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 600_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        }],
        ..empty_config()
    }
}

async fn start_echo_upstream() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("echo upstream bind");
    let address = listener.local_addr().expect("echo upstream address");
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.expect("echo upstream accept");
                    connections.spawn(async move {
                        let mut buffer = [0_u8; 1024];
                        loop {
                            let Ok(count) = stream.read(&mut buffer).await else { return };
                            if count == 0 || stream.write_all(&buffer[..count]).await.is_err() {
                                return;
                            }
                        }
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    (address, task)
}

async fn assert_echo(stream: &mut TcpStream, payload: &[u8]) {
    stream.write_all(payload).await.expect("relay write");
    let mut response = vec![0_u8; payload.len()];
    timeout(WIRE_TIMEOUT, stream.read_exact(&mut response))
        .await
        .unwrap_or_else(|_| panic!("relay response timeout for {payload:?}"))
        .expect("relay response");
    assert_eq!(response, payload);
}

fn http_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
    retirement: Retirement,
) -> ConfigDraft {
    let route = |path: &str, action| HttpRoute {
        host: None,
        path: HttpPathSelector::Exact { value: path.into() },
        methods: Vec::new(),
        access_policy: None,
        policy: HttpRoutePolicy::default(),
        action,
    };
    let mut config = tcp_config(
        management_address,
        listener_address,
        upstream_address,
        retirement,
    );
    config.listeners[0].protocol = Protocol::Http;
    config.listeners[0].service = Some("web".into());
    config.l4_services = Vec::new();
    config.http_services = vec![HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![
            route(
                "/rev",
                HttpRouteAction::FixedResponse {
                    status: 200,
                    body: "revision-0".into(),
                    headers: Vec::new(),
                },
            ),
            route(
                "/held",
                HttpRouteAction::Proxy {
                    upstream_pool: "upstream".into(),
                    policy: HttpProxyPolicy::default(),
                },
            ),
        ],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 30_000,
        max_request_body_bytes: Some(1_024),
        gzip: None,
        access_log: None,
    }];
    config
}

/// Answers each request only after a permit is added to `release`.
async fn start_held_http_upstream(release: Arc<Semaphore>) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("held upstream bind");
    let address = listener.local_addr().expect("held upstream address");
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.expect("held upstream accept");
                    let release = Arc::clone(&release);
                    connections.spawn(async move {
                        let mut head = Vec::new();
                        let mut byte = [0_u8; 1];
                        while !head.ends_with(b"\r\n\r\n") {
                            if stream.read_exact(&mut byte).await.is_err() {
                                return;
                            }
                            head.push(byte[0]);
                        }
                        release.acquire().await.expect("release gate").forget();
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nheld")
                            .await;
                        let _ = stream.read(&mut byte).await;
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    (address, task)
}

async fn send_get(stream: &mut TcpStream, path: &str) {
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("request");
}

async fn read_response(stream: &mut TcpStream) -> http_support::HttpResponse {
    let mut raw = Vec::new();
    let mut byte = [0_u8; 1];
    let (header_end, length) = loop {
        timeout(WIRE_TIMEOUT, stream.read_exact(&mut byte))
            .await
            .expect("response timeout")
            .expect("response byte");
        raw.push(byte[0]);
        if let Some(end) = raw.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    while raw.len() < header_end + length {
        timeout(WIRE_TIMEOUT, stream.read_exact(&mut byte))
            .await
            .expect("body timeout")
            .expect("body byte");
        raw.push(byte[0]);
    }
    http_support::HttpResponse::parse(raw)
}

async fn assert_closed(stream: &mut TcpStream) {
    let mut byte = [0_u8; 1];
    let read = timeout(Duration::from_secs(3), stream.read(&mut byte))
        .await
        .expect("the connection stayed open");
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "unexpected data after retirement: {read:?}"
    );
}

#[tokio::test]
async fn an_idle_http_keepalive_connection_is_closed_at_retirement() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start_http(Retirement::default()).await;
    let mut idle = TcpStream::connect(instance.listener)
        .await
        .expect("connection");
    send_get(&mut idle, "/rev").await;
    assert_eq!(read_response(&mut idle).await.body, b"revision-0");

    instance.apply().await;
    assert_closed(&mut idle).await;
    let mut fresh = TcpStream::connect(instance.listener)
        .await
        .expect("connection");
    send_get(&mut fresh, "/rev").await;
    assert_eq!(read_response(&mut fresh).await.body, b"revision-1");

    let status = instance.apply().await;
    assert!(
        status["retiredGenerations"].as_array().unwrap().is_empty(),
        "{status}"
    );
    assert!(status["pendingApply"].is_null());
    assert_closed(&mut fresh).await;
    instance.stop();
}

#[tokio::test]
async fn an_in_flight_http_response_finishes_and_closes_the_connection() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start_http(Retirement::default()).await;
    let mut busy = TcpStream::connect(instance.listener)
        .await
        .expect("connection");
    send_get(&mut busy, "/held").await;
    sleep(Duration::from_millis(300)).await;

    let status = instance.apply().await;
    let retired = &status["retiredGenerations"][0];
    assert_eq!(retired["inFlight"], 1, "{status}");
    assert_eq!(retired["inFlightByKind"]["http1"], 1);

    instance.release.add_permits(1);
    let response = read_response(&mut busy).await;
    assert_eq!(response.body, b"held");
    assert_eq!(
        response.headers.get("connection").map(String::as_str),
        Some("close")
    );
    assert_closed(&mut busy).await;
    wait_until("the retired generation drains", || async {
        let status = instance.generation().await;
        status["retiredGenerations"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    })
    .await;
    instance.stop();
}

#[tokio::test]
async fn supervised_idle_http_keepalive_is_closed_at_retirement() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start_http_mode(Retirement::default(), true).await;
    let mut idle = TcpStream::connect(instance.listener)
        .await
        .expect("connection");
    send_get(&mut idle, "/rev").await;
    assert_eq!(read_response(&mut idle).await.body, b"revision-0");

    instance.apply().await;
    assert_closed(&mut idle).await;
    // The retired worker leaves at once, so the next apply is not held up.
    let started = Instant::now();
    instance.apply().await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    instance.stop();
}

#[tokio::test]
async fn supervised_relay_is_closed_when_the_drain_deadline_passes() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start_tcp(
        Retirement {
            drain_deadline_ms: Some(2_000),
            max_retired_generations: 4,
        },
        true,
    )
    .await;
    let mut relay = open_relay(&instance).await;
    let applied = Instant::now();
    instance.apply().await;
    assert_echo(&mut relay, b"before the deadline").await;

    let mut buffer = [0_u8; 16];
    let closed = timeout(Duration::from_secs(10), relay.read(&mut buffer))
        .await
        .expect("the relay was never closed");
    assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");
    let elapsed = applied.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1_800),
        "closed too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "closed too late: {elapsed:?}"
    );
    instance.stop();
}

#[tokio::test]
async fn supervised_relay_outlives_a_second_apply_within_the_deadline() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start_tcp(Retirement::default(), true).await;
    let mut relay = open_relay(&instance).await;
    instance.apply().await;
    assert_echo(&mut relay, b"first apply").await;
    instance.write_next_revision();
    sleep(Duration::from_secs(1)).await;
    assert_echo(&mut relay, b"second apply waits").await;
    drop(relay);
    wait_until(
        "the deferred apply activates once the worker drains",
        || async {
            let generation = instance.generation().await;
            (generation["candidateRevision"].is_null()).then_some(())
        },
    )
    .await;
    instance.stop();
}

#[tokio::test]
async fn supervised_retired_worker_is_listed_by_the_active_worker_until_it_drains() {
    let _guard = process_test_guard().await;
    let mut instance = Instance::start_tcp(Retirement::default(), true).await;
    let mut relay = open_relay(&instance).await;
    let old_revision = instance.generation().await["activeRevision"].clone();
    instance.apply().await;

    let status = wait_until("the retired worker is listed", || async {
        let status = instance.generation().await;
        (!status["retiredGenerations"].as_array()?.is_empty()).then_some(status)
    })
    .await;
    let retired = &status["retiredGenerations"][0];
    assert_eq!(retired["revision"], old_revision, "{status}");
    assert_eq!(retired["state"], "draining");
    assert!(retired["inFlight"].as_u64().unwrap() >= 1, "{status}");
    assert!(retired["deadlineRemainingSeconds"].as_u64().unwrap() > 10_000);

    assert_echo(&mut relay, b"still served").await;
    drop(relay);
    wait_until("the retired worker leaves the list", || async {
        let status = instance.generation().await;
        status["retiredGenerations"]
            .as_array()?
            .is_empty()
            .then_some(())
    })
    .await;
    instance.stop();
}
