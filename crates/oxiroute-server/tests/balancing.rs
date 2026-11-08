//! Mixed-duration fairness: real proxy, real pools and stub origins that serve one request at a time.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/http.rs"]
mod http_support;

use std::{
    net::SocketAddr,
    os::fd::AsRawFd as _,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy,
    HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy, Listener, PoolResilience, Protocol,
    UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool, UpstreamServer,
};
use oxiroute_server::{
    HttpDownstreamPolicyApp, HttpReverseProxy, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RoundRobinPool,
    RuntimeMetrics, ServiceKind,
};
use pingora::{
    apps::ServerApp,
    protocols::{GetSocketDigest as _, SocketDigest},
    proxy::http_proxy,
    server::configuration::ServerConf,
};
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{raw_http_request, read_request_head};

const SHORT_MS: u64 = 50;
const LONG_MS: u64 = 800;
const SAMPLE: Duration = Duration::from_millis(10);

/// An origin that serves one request at a time and records when it was busy.
struct Stub {
    address: SocketAddr,
    busy: Arc<AtomicBool>,
    overlapped: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Stub {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("stub bind");
        let address = listener.local_addr().expect("stub address");
        let busy = Arc::new(AtomicBool::new(false));
        let overlapped = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicUsize::new(0));
        let (busy_flag, overlap_flag, served_count) = (
            Arc::clone(&busy),
            Arc::clone(&overlapped),
            Arc::clone(&served),
        );
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("stub accept");
                tokio::spawn(serve(
                    stream,
                    Arc::clone(&busy_flag),
                    Arc::clone(&overlap_flag),
                    Arc::clone(&served_count),
                ));
            }
        });
        Self {
            address,
            busy,
            overlapped,
            served,
            task,
        }
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut stream: TcpStream,
    busy: Arc<AtomicBool>,
    overlapped: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
) {
    while let Ok(head) = read_request_head(&mut stream).await {
        let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
        let sleep_ms = head
            .lines()
            .find_map(|line| line.strip_prefix("x-sleep-ms:"))
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0);
        if busy.swap(true, Ordering::SeqCst) {
            overlapped.store(true, Ordering::SeqCst);
        }
        sleep(Duration::from_millis(sleep_ms)).await;
        busy.store(false, Ordering::SeqCst);
        served.fetch_add(1, Ordering::SeqCst);
        if stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .is_err()
        {
            return;
        }
    }
}

struct Balancer {
    address: SocketAddr,
    pool: Arc<RoundRobinPool>,
    stubs: Vec<Stub>,
    _generation: Arc<oxiroute_server::RuntimeGeneration>,
    _shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Balancer {
    #[allow(clippy::too_many_lines)]
    async fn start(
        algorithm: UpstreamAlgorithm,
        reuse: UpstreamConnectionReuse,
        capacities: &[u64],
        expected_connections: usize,
    ) -> Self {
        let mut stubs = Vec::new();
        for _ in capacities {
            stubs.push(Stub::start().await);
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let address = listener.local_addr().expect("proxy address");
        drop(listener);
        let servers = stubs
            .iter()
            .zip(capacities)
            .enumerate()
            .map(|(index, (stub, capacity))| UpstreamServer {
                name: format!("s{index}"),
                endpoint: socket_endpoint(stub.address),
                max_connections: Some(*capacity),
                dns_resolution: DnsResolutionPolicy::OnConnect,
            })
            .collect();
        let config = ConfigDraft {
            retirement: None,
            listeners: vec![Listener {
                name: "http-loopback".into(),
                bind: socket_bind(address),
                protocol: Protocol::Http,
                service: Some("balance".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: Some(1_000),
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
            upstream_pools: vec![UpstreamPool {
                name: "workers".into(),
                servers,
                endpoints: Vec::new(),
                algorithm,
                health_check: None,
                passive_health: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: Some(30_000),
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: reuse,
                registration: None,
                canary: None,
                resilience: PoolResilience::default(),
                queue: None,
            }],
            http_services: vec![HttpService {
                drain_deadline_ms: None,
                name: "balance".into(),
                routes: vec![HttpRoute {
                    host: None,
                    path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::Proxy {
                        upstream_pool: "workers".into(),
                        policy: HttpProxyPolicy::default().into(),
                    },
                }],
                automatic_response_headers: false,
                upstream_io_timeout_ms: 30_000,
                max_request_body_bytes: Some(1024),
                gzip: None,
                access_log: None,
            }],
            ..empty_config()
        };
        let config = config.validate().expect("valid balancing config");
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
            let mut connections = Vec::new();
            for _ in 0..expected_connections {
                let (stream, _) = listener.accept().await.expect("proxy accept");
                let Some(admission) = proxy.admit_connection() else {
                    continue;
                };
                let mut stream = pingora::protocols::l4::stream::Stream::from(stream);
                stream.set_socket_digest(SocketDigest::from_raw_fd(stream.as_raw_fd()));
                let stream: pingora::protocols::Stream = Box::new(stream);
                let proxy = Arc::clone(&proxy);
                let shutdown = shutdown.clone();
                connections.push(tokio::spawn(async move {
                    let _admission = admission;
                    proxy.process_new(stream, &shutdown).await;
                }));
            }
            for connection in connections {
                connection.await.expect("proxy connection task");
            }
        });
        Self {
            address,
            pool,
            stubs,
            _generation: generation,
            _shutdown: shutdown_tx,
            task,
        }
    }
}

impl Drop for Balancer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One request of the workload: it is sent `at_ms` after the start and keeps a stub busy `work_ms`.
#[derive(Clone, Copy)]
struct Job {
    at_ms: u64,
    work_ms: u64,
}

struct Outcome {
    makespan: Duration,
    /// Samples where a stub sat idle although more requests were outstanding than stubs working.
    idle_while_waiting: usize,
    served: Vec<usize>,
    pool: Arc<RoundRobinPool>,
    overlapped: bool,
}

async fn run(
    algorithm: UpstreamAlgorithm,
    reuse: UpstreamConnectionReuse,
    capacities: &[u64],
    jobs: &[Job],
) -> Outcome {
    let balancer = Balancer::start(algorithm, reuse, capacities, jobs.len()).await;
    let started = Instant::now();
    let outstanding = Arc::new(AtomicUsize::new(0));
    let sampler = {
        let outstanding = Arc::clone(&outstanding);
        let flags = balancer
            .stubs
            .iter()
            .map(|stub| Arc::clone(&stub.busy))
            .collect::<Vec<_>>();
        tokio::spawn(async move {
            let mut violations = 0;
            loop {
                sleep(SAMPLE).await;
                let working = flags
                    .iter()
                    .filter(|busy| busy.load(Ordering::SeqCst))
                    .count();
                if working < flags.len() && outstanding.load(Ordering::SeqCst) > working {
                    violations += 1;
                }
                if Arc::strong_count(&outstanding) == 1 {
                    return violations;
                }
            }
        })
    };
    let requests = jobs
        .iter()
        .map(|job| {
            let (address, job, outstanding) = (balancer.address, *job, Arc::clone(&outstanding));
            tokio::spawn(async move {
                sleep(Duration::from_millis(job.at_ms)).await;
                outstanding.fetch_add(1, Ordering::SeqCst);
                let request = format!(
                    "GET /work HTTP/1.1\r\nHost: balance.test\r\nx-sleep-ms: {}\r\nConnection: close\r\n\r\n",
                    job.work_ms
                );
                let response = raw_http_request(address, request.as_bytes()).await;
                outstanding.fetch_sub(1, Ordering::SeqCst);
                response.status
            })
        })
        .collect::<Vec<_>>();
    for request in requests {
        let status = timeout(Duration::from_secs(60), request)
            .await
            .expect("request finished")
            .expect("request task");
        assert_eq!(status, 200);
    }
    let makespan = started.elapsed();
    drop(outstanding);
    let idle_while_waiting = sampler.await.expect("sampler task");
    Outcome {
        makespan,
        idle_while_waiting,
        served: balancer
            .stubs
            .iter()
            .map(|stub| stub.served.load(Ordering::SeqCst))
            .collect(),
        overlapped: balancer
            .stubs
            .iter()
            .any(|stub| stub.overlapped.load(Ordering::SeqCst)),
        pool: Arc::clone(&balancer.pool),
    }
}

/// One long job arrives first, then a burst of short jobs; ideal makespan is the work over the stubs.
fn long_first_burst(shorts: usize) -> (Vec<Job>, Duration) {
    let mut jobs = vec![Job {
        at_ms: 0,
        work_ms: LONG_MS,
    }];
    jobs.extend((0..shorts).map(|_| Job {
        at_ms: 100,
        work_ms: SHORT_MS,
    }));
    let work = LONG_MS + SHORT_MS * u64::try_from(shorts).expect("short count");
    (jobs, Duration::from_millis(work / 2))
}

fn algorithms() -> Vec<(&'static str, UpstreamAlgorithm)> {
    vec![
        ("round_robin", UpstreamAlgorithm::RoundRobin),
        (
            "weighted_round_robin",
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![1, 1],
            },
        ),
        ("least_connections", UpstreamAlgorithm::LeastConnections),
        ("power_of_two_choices", UpstreamAlgorithm::PowerOfTwoChoices),
        ("first", UpstreamAlgorithm::First),
    ]
}

fn assert_late_binding(label: &str, outcome: &Outcome, ideal: Duration) {
    assert!(
        !outcome.overlapped,
        "{label}: a stub served two requests at once"
    );
    assert!(
        outcome.makespan <= ideal.mul_f64(1.25) + Duration::from_millis(200),
        "{label}: makespan {:?} exceeds 1.25x the ideal {ideal:?}; served {:?}",
        outcome.makespan,
        outcome.served
    );
    assert!(
        outcome.idle_while_waiting <= 20,
        "{label}: a stub sat idle in {} samples while requests waited",
        outcome.idle_while_waiting
    );
    assert_eq!(
        outcome.pool.health_snapshot().queued,
        0,
        "{label}: queue must drain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_requests_use_whichever_server_frees_first_on_every_algorithm() {
    let (jobs, ideal) = long_first_burst(24);
    for (name, algorithm) in algorithms() {
        for (mode, reuse) in [
            ("safe", UpstreamConnectionReuse::Safe),
            ("never", UpstreamConnectionReuse::Never),
        ] {
            let outcome = run(algorithm.clone(), reuse, &[1, 1], &jobs).await;
            assert_late_binding(&format!("{name}/{mode}"), &outcome, ideal);
            if mode == "never" {
                assert!(
                    outcome
                        .pool
                        .health_snapshot()
                        .endpoints
                        .iter()
                        .all(|endpoint| endpoint.active_connections == 0),
                    "{name}/{mode}: leaked permits"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_spills_to_the_second_server_whenever_the_first_is_at_capacity() {
    let (jobs, _) = long_first_burst(12);
    let outcome = run(
        UpstreamAlgorithm::First,
        UpstreamConnectionReuse::Safe,
        &[1, 1],
        &jobs,
    )
    .await;
    assert!(
        outcome.served[1] >= 6,
        "the second server served only {} of {} requests",
        outcome.served[1],
        jobs.len()
    );
}
