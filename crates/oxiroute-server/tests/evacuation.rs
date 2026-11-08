//! Evacuation: real proxy, real pools and localhost origins that the test releases explicitly.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/http.rs"]
mod http_support;

use std::{
    net::SocketAddr,
    os::fd::AsRawFd as _,
    path::PathBuf,
    sync::{Arc, Barrier},
    time::Duration,
};

use oxiroute_config::{
    AccessLogPolicy, ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpPathSelector,
    HttpProxyPolicy, HttpRetryBodySafety, HttpRetryBudget, HttpRetryMethodSafety, HttpRetryPolicy,
    HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy, Listener,
    PoolResilience, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
    UpstreamServer,
};
use oxiroute_server::{
    AdministrativeState, EvacuationReport, HttpDownstreamPolicyApp, HttpReverseProxy, LeftRunning,
    MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RoundRobinPool, RuntimeMetrics, ServiceKind,
};
use pingora::{
    apps::ServerApp,
    protocols::{GetSocketDigest as _, SocketDigest},
    proxy::http_proxy,
    server::configuration::ServerConf,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{HttpResponse, http_request};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);
const BODY: &str = "payload-123";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    /// Answers as soon as the request arrived.
    Reply,
    /// Reads the request, then waits for the test to release it.
    Hold,
    /// Sends the response header and the first bytes at once, the rest when released.
    Stream,
}

#[derive(Debug, Eq, PartialEq)]
enum Event {
    Arrived {
        request_id: String,
        body: String,
    },
    /// The proxy closed the connection before the origin answered.
    Dropped {
        request_id: String,
    },
}

/// A localhost origin that stays bound for the whole test and reports what it sees.
struct Origin {
    address: SocketAddr,
    events: mpsc::UnboundedReceiver<Event>,
    gate: Arc<Semaphore>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn start(name: String, mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let (events_tx, events) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        let served = Arc::clone(&gate);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                let connection = Connection {
                    name: name.clone(),
                    mode,
                    gate: Arc::clone(&served),
                    events: events_tx.clone(),
                };
                tokio::spawn(connection.serve(stream));
            }
        });
        Self {
            address,
            events,
            gate,
            task,
        }
    }

    /// Lets `count` held requests answer.
    fn release(&self, count: usize) {
        self.gate.add_permits(count);
    }

    async fn next(&mut self) -> Event {
        timeout(TEST_TIMEOUT, self.events.recv())
            .await
            .expect("origin event in time")
            .expect("origin task alive")
    }

    async fn arrival(&mut self, request_id: &str) -> String {
        match self.next().await {
            Event::Arrived {
                request_id: id,
                body,
            } if id == request_id => body,
            other => panic!("expected {request_id} to arrive, got {other:?}"),
        }
    }

    async fn dropped(&mut self, request_id: &str) {
        assert_eq!(
            self.next().await,
            Event::Dropped {
                request_id: request_id.into()
            }
        );
    }

    /// Everything the origin reported meanwhile.
    fn seen(&mut self) -> Vec<Event> {
        std::iter::from_fn(|| self.events.try_recv().ok()).collect()
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Connection {
    name: String,
    mode: Mode,
    gate: Arc<Semaphore>,
    events: mpsc::UnboundedSender<Event>,
}

impl Connection {
    async fn serve(self, mut stream: TcpStream) {
        while let Some((request_id, body)) = read_request(&mut stream).await {
            self.events
                .send(Event::Arrived {
                    request_id: request_id.clone(),
                    body,
                })
                .expect("test alive");
            let proceeded = match self.mode {
                Mode::Reply => true,
                Mode::Hold => self.released(&mut stream, &request_id).await,
                Mode::Stream => self.stream(&mut stream, &request_id).await,
            };
            if !proceeded {
                return;
            }
            if self.mode != Mode::Stream {
                let body = format!("from-{}", self.name);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Origin: {}\r\n\r\n",
                    body.len(),
                    self.name
                );
                if stream
                    .write_all(format!("{head}{body}").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }

    async fn stream(&self, stream: &mut TcpStream, request_id: &str) -> bool {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nX-Origin: {}\r\n\r\nhello",
            self.name
        );
        if stream.write_all(head.as_bytes()).await.is_err()
            || !self.released(stream, request_id).await
        {
            return false;
        }
        stream.write_all(b"world").await.is_ok()
    }

    /// Waits for the test to release the request; false when the proxy closed the connection first.
    async fn released(&self, stream: &mut TcpStream, request_id: &str) -> bool {
        let mut byte = [0; 1];
        tokio::select! {
            permit = self.gate.acquire() => {
                permit.expect("gate open").forget();
                true
            }
            _ = stream.read(&mut byte) => {
                self.events.send(Event::Dropped { request_id: request_id.into() }).expect("test alive");
                false
            }
        }
    }
}

/// The request id and body of the next request on the connection.
async fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut received = Vec::new();
    let mut buffer = [0; 1024];
    let head_end = loop {
        if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
    let header = |name: &str| {
        head.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_owned())
    };
    let length =
        header("content-length:").map_or(0, |value| value.parse::<usize>().expect("length"));
    while received.len() < head_end + length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&received[head_end..head_end + length]).into_owned();
    Some((header("x-request-id:").unwrap_or_default(), body))
}

/// How the pool, the route and the origins are set up.
struct Layout {
    pool: &'static str,
    modes: Vec<Mode>,
    capacity: Option<u64>,
    queue_ms: Option<u64>,
    replayable_posts: bool,
    access_log: bool,
}

impl Layout {
    /// A pool named after the test, so that tests of one process never evacuate each other's requests.
    fn new(pool: &'static str, modes: &[Mode]) -> Self {
        Self {
            pool,
            modes: modes.to_vec(),
            capacity: None,
            queue_ms: None,
            replayable_posts: false,
            access_log: false,
        }
    }

    fn capacity(mut self, capacity: u64) -> Self {
        self.capacity = Some(capacity);
        self
    }

    fn queue(mut self) -> Self {
        self.queue_ms = Some(30_000);
        self
    }

    fn replayable_posts(mut self) -> Self {
        self.replayable_posts = true;
        self
    }

    fn access_log(mut self) -> Self {
        self.access_log = true;
        self
    }

    /// No retries at all: evacuation must work with `max_retries = 0` and spend no retry budget.
    fn retry(&self) -> HttpRetryPolicy {
        let posts = self.replayable_posts;
        HttpRetryPolicy {
            max_retries: 0,
            method_safety: if posts {
                HttpRetryMethodSafety::All
            } else {
                HttpRetryMethodSafety::GetHead
            },
            body_safety: if posts {
                HttpRetryBodySafety::Buffered
            } else {
                HttpRetryBodySafety::Empty
            },
            idempotency_header: posts.then(|| "idempotency-key".to_owned()),
            budget: Some(HttpRetryBudget {
                per_second: Some(1),
                ratio_percent: None,
            }),
            ..HttpRetryPolicy::default()
        }
    }
}

fn server_name(index: usize) -> String {
    format!(
        "node-{}",
        char::from(b'a' + u8::try_from(index).expect("few servers"))
    )
}

struct Farm {
    address: SocketAddr,
    pool: Arc<RoundRobinPool>,
    origins: Vec<Origin>,
    access_log: PathBuf,
    _directory: TempDir,
    _generation: Arc<oxiroute_server::RuntimeGeneration>,
    _shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Farm {
    async fn start(layout: Layout) -> Self {
        let mut origins = Vec::new();
        for (index, mode) in layout.modes.iter().enumerate() {
            origins.push(Origin::start(server_name(index), *mode).await);
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let address = listener.local_addr().expect("proxy address");
        drop(listener);
        let directory = TempDir::new().expect("access log directory");
        let access_log = directory.path().join("access.jsonl");
        let config = config(
            &layout,
            address,
            &origins,
            layout.access_log.then(|| access_log.clone()),
        );
        let config = config.validate().expect("valid evacuation config");
        let generation = config_support::runtime_generation(&config).expect("runtime generation");
        let listener = std::net::TcpListener::from(
            generation
                .reservations()
                .get("http-loopback")
                .expect("listener reservation")
                .duplicate_owned_fd()
                .expect("duplicate listener"),
        );
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        let pool = Arc::clone(&generation.pools()[0]);
        let spec = generation.services().to_vec().remove(0);
        let metrics = RuntimeMetrics::new();
        let listener_metrics = metrics
            .register_listener(
                &spec.name,
                spec.kind.protocol(),
                spec.bind.to_string(),
                spec.max_connections.unwrap_or(u64::MAX),
            )
            .expect("listener metrics");
        let downstream_timeouts = spec.downstream_timeouts;
        let ServiceKind::Http(service) = spec.kind else {
            panic!("HTTP service expected");
        };
        let configuration = Arc::new(ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        });
        let proxy = Arc::new(MonitoredHttpApp::new(
            HttpDownstreamPolicyApp::new(
                http_proxy(
                    &configuration,
                    HttpReverseProxy::new(service, listener_metrics.clone())
                        .with_generation(Arc::clone(&generation)),
                ),
                downstream_timeouts,
            ),
            listener_metrics,
            Arc::clone(&generation),
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("proxy accept");
                let Some(admission) = proxy.admit_connection() else {
                    continue;
                };
                let mut stream = pingora::protocols::l4::stream::Stream::from(stream);
                stream.set_socket_digest(SocketDigest::from_raw_fd(stream.as_raw_fd()));
                let stream: pingora::protocols::Stream = Box::new(stream);
                let proxy = Arc::clone(&proxy);
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    let _admission = admission;
                    proxy.process_new(stream, &shutdown).await;
                });
            }
        });
        Self {
            address,
            pool,
            origins,
            access_log,
            _directory: directory,
            _generation: generation,
            _shutdown: shutdown_tx,
            task,
        }
    }

    fn get(&self, request_id: &str) -> JoinHandle<HttpResponse> {
        self.request("GET", request_id, &[], b"")
    }

    fn request(
        &self,
        method: &str,
        request_id: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> JoinHandle<HttpResponse> {
        let address = self.address;
        let method = method.to_owned();
        let headers = std::iter::once(("X-Request-Id", request_id))
            .chain(headers.iter().copied())
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        let body = body.to_vec();
        tokio::spawn(async move {
            let headers = headers
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str()))
                .collect::<Vec<_>>();
            http_request(address, &method, "/render", &headers, &body).await
        })
    }

    fn evacuate(&self, server: &str, force: bool) -> EvacuationReport {
        self.pool
            .evacuate_server(server, force)
            .expect("known server")
    }

    /// The access log lines written so far, waiting for at least `lines` of them.
    async fn access_events(&self, lines: usize) -> Vec<serde_json::Value> {
        timeout(TEST_TIMEOUT, async {
            loop {
                let content = std::fs::read_to_string(&self.access_log).unwrap_or_default();
                if content.lines().count() >= lines {
                    return content
                        .lines()
                        .map(|line| serde_json::from_str(line).expect("log line"))
                        .collect();
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("access log lines in time")
    }
}

impl Drop for Farm {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn config(
    layout: &Layout,
    address: SocketAddr,
    origins: &[Origin],
    access_log: Option<PathBuf>,
) -> ConfigDraft {
    let servers = origins
        .iter()
        .enumerate()
        .map(|(index, origin)| UpstreamServer {
            name: server_name(index),
            endpoint: socket_endpoint(origin.address),
            max_connections: layout.capacity,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        })
        .collect();
    ConfigDraft {
        retirement: None,
        listeners: vec![Listener {
            name: "http-loopback".into(),
            bind: socket_bind(address),
            protocol: Protocol::Http,
            service: Some("evacuation".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(1_000),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: layout.pool.into(),
            servers,
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::First,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: layout.queue_ms,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: PoolResilience::default(),
            queue: None,
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "evacuation".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy {
                    request_buffering: layout.replayable_posts,
                    ..HttpRoutePolicy::default()
                },
                action: HttpRouteAction::Proxy {
                    upstream_pool: layout.pool.into(),
                    policy: HttpProxyPolicy {
                        retry: layout.retry(),
                        ..HttpProxyPolicy::default()
                    }
                    .into(),
                },
            }],
            automatic_response_headers: false,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(1024),
            gzip: None,
            access_log: access_log.map(|path| AccessLogPolicy::File { path }),
        }],
        ..empty_config()
    }
}

async fn wait_for_live_phase(request_id: &str, phase: &str) {
    timeout(TEST_TIMEOUT, async {
        while !oxiroute_server::live_requests()
            .iter()
            .any(|(id, live)| id == request_id && live == phase)
        {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("request {request_id} never reached {phase}"));
}

fn assert_report(
    report: &EvacuationReport,
    redispatched: u64,
    forced: u64,
    left: &[(LeftRunning, u64)],
) {
    assert_eq!(report.redispatched, redispatched, "{report:?}");
    assert_eq!(report.forced, forced, "{report:?}");
    assert_eq!(
        report.left_running,
        left.iter().copied().collect(),
        "{report:?}"
    );
}

fn assert_served_by(response: &HttpResponse, server: &str, request_id: &str) {
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(
        response.header("x-origin"),
        Some(server),
        "{}",
        response.text()
    );
    assert_eq!(response.body(), format!("from-{server}").as_bytes());
    assert_eq!(response.header("x-request-id"), Some(request_id));
}

#[tokio::test]
async fn a_held_get_moves_to_another_server_with_the_same_request_id() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-get", &[Mode::Hold, Mode::Reply]).access_log()).await;
        let client = farm.get("moved-get");
        farm.origins[0].arrival("moved-get").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        farm.origins[0].dropped("moved-get").await;
        farm.origins[1].arrival("moved-get").await;
        assert_served_by(&client.await.expect("client"), "node-b", "moved-get");
        let health = farm.pool.health_snapshot();
        assert_eq!(
            health.endpoints[0].administrative_state,
            AdministrativeState::Drain
        );
        let event = &farm.access_events(1).await[0];
        assert_eq!(event["requestId"], "moved-get");
        assert_eq!(event["status"], 200);
        assert_eq!(event["servers"], "node-a,node-b");
        assert_eq!(event["attempts"], 1, "the move is not a try");
        assert_eq!(event["evacuations"], 1);
        assert_eq!(event["reason"], "");
    })
    .await
    .expect("moved get test timed out");
}

#[tokio::test]
async fn a_post_with_an_idempotency_key_moves_with_its_whole_body() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-post", &[Mode::Hold, Mode::Reply]).replayable_posts())
                .await;
        let client = farm.request(
            "POST",
            "moved-post",
            &[("Idempotency-Key", "k-1")],
            BODY.as_bytes(),
        );
        assert_eq!(farm.origins[0].arrival("moved-post").await, BODY);

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        assert_eq!(farm.origins[1].arrival("moved-post").await, BODY);
        assert_served_by(&client.await.expect("client"), "node-b", "moved-post");
    })
    .await
    .expect("moved post test timed out");
}

#[tokio::test]
async fn a_response_already_streaming_finishes_on_its_server() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-stream", &[Mode::Stream, Mode::Reply])).await;
        let mut client = TcpStream::connect(farm.address).await.expect("client connect");
        client
            .write_all(b"GET /render HTTP/1.1\r\nHost: t\r\nX-Request-Id: streaming\r\nConnection: close\r\n\r\n")
            .await
            .expect("client request");
        let mut received = Vec::new();
        while !received.ends_with(b"hello") {
            let mut buffer = [0; 256];
            let read = client.read(&mut buffer).await.expect("client read");
            assert_ne!(read, 0, "closed before the first bytes");
            received.extend_from_slice(&buffer[..read]);
        }

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 0, 0, &[(LeftRunning::ResponseStarted, 1)]);
        farm.origins[0].release(1);
        client.read_to_end(&mut received).await.expect("client finish");
        assert!(received.ends_with(b"helloworld"), "{}", String::from_utf8_lossy(&received));
        assert!(farm.origins[1].seen().is_empty());
    })
    .await
    .expect("streaming test timed out");
}

#[tokio::test]
async fn forcing_resets_a_response_that_already_started() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-reset", &[Mode::Stream, Mode::Reply])).await;
        let mut client = TcpStream::connect(farm.address).await.expect("client connect");
        client
            .write_all(b"GET /render HTTP/1.1\r\nHost: t\r\nX-Request-Id: reset\r\nConnection: close\r\n\r\n")
            .await
            .expect("client request");
        let mut received = Vec::new();
        while !received.ends_with(b"hello") {
            let mut buffer = [0; 256];
            let read = client.read(&mut buffer).await.expect("client read");
            assert_ne!(read, 0, "closed before the first bytes");
            received.extend_from_slice(&buffer[..read]);
        }

        let report = farm.evacuate("node-a", true);

        assert_report(&report, 0, 1, &[]);
        farm.origins[0].arrival("reset").await;
        farm.origins[0].dropped("reset").await;
        let _ = client.read_to_end(&mut received).await;
        assert!(!received.ends_with(b"world"), "{}", String::from_utf8_lossy(&received));
        assert!(farm.origins[1].seen().is_empty());
    })
    .await
    .expect("forced reset test timed out");
}

#[tokio::test]
async fn a_post_without_an_idempotency_key_stays_unless_forced() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-keyless", &[Mode::Hold, Mode::Reply]).replayable_posts())
                .await;
        let stays = farm.request("POST", "keyless", &[], BODY.as_bytes());
        farm.origins[0].arrival("keyless").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 0, 0, &[(LeftRunning::NotReplayable, 1)]);
        farm.origins[0].release(1);
        assert_served_by(&stays.await.expect("client"), "node-a", "keyless");
        assert!(farm.origins[0].seen().is_empty() && farm.origins[1].seen().is_empty());
    })
    .await
    .expect("keyless post test timed out");
}

#[tokio::test]
async fn forcing_a_post_without_an_idempotency_key_answers_the_evacuated_error() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-killed", &[Mode::Hold, Mode::Reply]).replayable_posts())
                .await;
        let client = farm.request("POST", "killed", &[], BODY.as_bytes());
        farm.origins[0].arrival("killed").await;

        let report = farm.evacuate("node-a", true);

        assert_report(&report, 0, 1, &[]);
        farm.origins[0].dropped("killed").await;
        let response = client.await.expect("client");
        assert_eq!(response.status, 503, "{}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("evacuated"));
        assert_eq!(response.header("x-request-id"), Some("killed"));
        assert!(
            !response.text().contains("node-"),
            "no server name reaches the caller"
        );
        assert!(farm.origins[1].seen().is_empty());
    })
    .await
    .expect("forced post test timed out");
}

#[tokio::test]
async fn the_only_server_keeps_its_requests() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-alone", &[Mode::Hold])).await;
        let client = farm.get("alone");
        farm.origins[0].arrival("alone").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 0, 0, &[(LeftRunning::NoDestination, 1)]);
        farm.origins[0].release(1);
        assert_served_by(&client.await.expect("client"), "node-a", "alone");
        assert!(farm.origins[0].seen().is_empty());
    })
    .await
    .expect("only server test timed out");
}

#[tokio::test]
async fn a_pool_queue_holds_the_moved_request_until_a_server_frees() {
    timeout(TEST_TIMEOUT, async {
        let layout = Layout::new("farm-queue", &[Mode::Hold, Mode::Hold])
            .capacity(1)
            .queue();
        let mut farm = Farm::start(layout).await;
        let moved = farm.get("moved-to-queue");
        farm.origins[0].arrival("moved-to-queue").await;
        let busy = farm.get("busy");
        farm.origins[1].arrival("busy").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        farm.origins[0].dropped("moved-to-queue").await;
        wait_for_live_phase("moved-to-queue", "queued").await;
        assert!(farm.origins[1].seen().is_empty());
        farm.origins[1].release(2);
        assert_served_by(&busy.await.expect("busy client"), "node-b", "busy");
        farm.origins[1].arrival("moved-to-queue").await;
        assert_served_by(
            &moved.await.expect("moved client"),
            "node-b",
            "moved-to-queue",
        );
    })
    .await
    .expect("queue test timed out");
}

#[tokio::test]
async fn a_request_waiting_in_the_queue_is_not_touched() {
    timeout(TEST_TIMEOUT, async {
        let layout = Layout::new("farm-waiting", &[Mode::Hold, Mode::Hold])
            .capacity(1)
            .queue();
        let mut farm = Farm::start(layout).await;
        let first = farm.get("first");
        farm.origins[0].arrival("first").await;
        let second = farm.get("second");
        farm.origins[1].arrival("second").await;
        let waiting = farm.get("waiting");
        wait_for_live_phase("waiting", "queued").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        farm.origins[0].dropped("first").await;
        farm.origins[1].release(3);
        assert_served_by(&second.await.expect("second"), "node-b", "second");
        assert_served_by(&waiting.await.expect("waiting"), "node-b", "waiting");
        assert_served_by(&first.await.expect("first"), "node-b", "first");
    })
    .await
    .expect("waiting request test timed out");
}

#[tokio::test]
async fn a_request_is_moved_three_times_and_the_fourth_stays() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-chain", &[Mode::Hold; 4])).await;
        let client = farm.get("chain");
        farm.origins[0].arrival("chain").await;

        for hop in 0..3 {
            let report = farm.evacuate(&server_name(hop), false);
            assert_report(&report, 1, 0, &[]);
            farm.origins[hop].dropped("chain").await;
            farm.origins[hop + 1].arrival("chain").await;
        }
        let report = farm.evacuate("node-d", false);

        assert_report(&report, 0, 0, &[(LeftRunning::LimitReached, 1)]);
        farm.origins[3].release(1);
        assert_served_by(&client.await.expect("client"), "node-d", "chain");
    })
    .await
    .expect("chain test timed out");
}

#[tokio::test]
async fn the_drain_outlives_the_evacuation_and_ready_ends_it() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-drain", &[Mode::Hold, Mode::Reply])).await;
        let moved = farm.get("drained-first");
        farm.origins[0].arrival("drained-first").await;
        farm.evacuate("node-a", false);
        farm.origins[0].dropped("drained-first").await;
        assert_served_by(&moved.await.expect("moved"), "node-b", "drained-first");

        assert_served_by(
            &farm.get("while-drained").await.expect("client"),
            "node-b",
            "while-drained",
        );
        farm.pool
            .set_server_administrative_state("node-a", AdministrativeState::Ready)
            .expect("ready");
        farm.origins[0].release(1);
        assert_served_by(
            &farm.get("after-ready").await.expect("client"),
            "node-a",
            "after-ready",
        );
    })
    .await
    .expect("drain persistence test timed out");
}

/// Evacuates at the moment the origin lets the response go: whichever side wins, the caller gets
/// exactly one complete response and the origin that did not answer never sees a second request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evacuating_while_the_response_header_arrives_never_hangs_or_duplicates() {
    for round in 0..15 {
        timeout(TEST_TIMEOUT, async {
            let mut farm = Farm::start(Layout::new("farm-race", &[Mode::Hold, Mode::Reply])).await;
            let id = format!("race-{round}");
            let client = farm.get(&id);
            farm.origins[0].arrival(&id).await;
            let barrier = Arc::new(Barrier::new(2));
            let gate = Arc::clone(&farm.origins[0].gate);
            let release = {
                let barrier = Arc::clone(&barrier);
                tokio::task::spawn_blocking(move || {
                    barrier.wait();
                    gate.add_permits(1);
                })
            };
            let pool = Arc::clone(&farm.pool);
            let evacuate = tokio::task::spawn_blocking(move || {
                barrier.wait();
                pool.cancel_server_requests("node-a", false)
                    .expect("known server")
            });
            release.await.expect("release");
            let report = evacuate.await.expect("evacuate");

            let response = client.await.expect("client");
            let moved = report.redispatched == 1;
            // A request that already finished is no candidate any more, which is a third valid outcome.
            assert!(
                report.redispatched + report.left_running_total() <= 1,
                "{report:?}"
            );
            assert_served_by(&response, if moved { "node-b" } else { "node-a" }, &id);
            let arrivals = farm.origins[1].seen();
            assert_eq!(arrivals.len(), usize::from(moved), "{arrivals:?}");
        })
        .await
        .expect("race round timed out");
    }
}
