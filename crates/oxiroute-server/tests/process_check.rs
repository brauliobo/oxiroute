//! `oxiroute check` against a real scratch process: every Nagios exit code, driven by real faults.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[allow(dead_code)]
#[path = "support/process.rs"]
mod process_support;

use std::{
    net::SocketAddr,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpService,
    Listener, Management, Protocol, Registry, RegistryEndpoint, RegistryRole, UpstreamPool,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
};

use config_support::{empty_config, socket_bind};
use process_support::{ServerProcess, reserve_tcp_address};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

struct Verdict {
    code: i32,
    stdout: String,
}

struct Scratch {
    state: TempDir,
    management: SocketAddr,
    traffic: SocketAddr,
}

impl Scratch {
    fn socket(&self) -> std::path::PathBuf {
        self.state.path().join("status.sock")
    }

    fn pool(servers: &[SocketAddr]) -> UpstreamPool {
        let servers = servers
            .iter()
            .zip(["alpha", "beta"])
            .map(|(address, name)| {
                json!({
                    "name": name,
                    "endpoint": {"type": "socket", "address": address.to_string()},
                    "max_connections": 4,
                })
            })
            .collect::<Vec<_>>();
        serde_json::from_value(json!({
            "name": "farm",
            "servers": servers,
            "health_check": {
                "type": "http", "path": "/", "expected_status": 200, "interval_ms": 1000,
                "timeout_ms": 500, "healthy_threshold": 1, "unhealthy_threshold": 1,
                "startup": "healthy",
            },
            "registration": {"allowed_cidrs": ["127.0.0.0/8"], "require_peer_address": false},
            "canary": {"path": "/deep", "interval_ms": 1000, "timeout_ms": 500},
        }))
        .expect("farm pool")
    }

    fn config(&self, servers: &[SocketAddr]) -> oxiroute_config::ValidatedConfig {
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::Proxy {
                upstream_pool: "farm".into(),
                policy: HttpProxyPolicy::default().into(),
            },
        };
        ConfigDraft {
            management: Some(Management {
                bind: self.management,
                ui_dir: None,
            }),
            registry: Some(Registry {
                state_dir: self.state.path().join("state"),
                tokens_file: None,
                endpoints: vec![RegistryEndpoint::Unix {
                    path: self.socket(),
                    mode: 0o660,
                    role: RegistryRole::Read,
                    pools: Vec::new(),
                }],
            }),
            listeners: vec![Listener {
                name: "traffic".into(),
                bind: socket_bind(self.traffic),
                protocol: Protocol::Http,
                service: Some("traffic".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: Some(100),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
            upstream_pools: vec![Self::pool(servers)],
            http_services: vec![HttpService {
                name: "traffic".into(),
                routes: vec![route],
                automatic_response_headers: true,
                upstream_io_timeout_ms: 2_000,
                max_request_body_bytes: Some(64 * 1024),
                gzip: None,
                access_log: None,
                drain_deadline_ms: None,
            }],
            ..empty_config()
        }
        .validate()
        .expect("valid check config")
    }

    async fn check(&self, words: &[&str]) -> Verdict {
        let mut arguments = vec![
            "--endpoint".to_owned(),
            format!("http://{}", self.management),
        ];
        arguments.push("check".into());
        arguments.extend(words.iter().map(|word| (*word).to_owned()));
        run_cli(arguments).await
    }
}

async fn run_cli(arguments: Vec<String>) -> Verdict {
    tokio::task::spawn_blocking(move || {
        let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .env_remove("OXIROUTE_ENDPOINT")
            .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
            .args(arguments)
            .output()
            .expect("oxiroute CLI");
        Verdict {
            code: output.status.code().expect("exit code"),
            stdout: String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned(),
        }
    })
    .await
    .expect("CLI task")
}

/// A backend that answers every request with 200, until it is aborted. `/deep`, the path of the
/// canary, answers 503 while `deep_healthy` is false.
async fn stub(deep_healthy: Arc<AtomicBool>) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("stub bind");
    let address = listener.local_addr().expect("stub address");
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let deep_healthy = Arc::clone(&deep_healthy);
            tokio::spawn(async move {
                let mut buffer = [0_u8; 2048];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let deep = buffer[..read].starts_with(b"GET /deep ");
                let status = if deep && !deep_healthy.load(Ordering::Relaxed) {
                    "503 Service Unavailable"
                } else {
                    "200 OK"
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    (address, task)
}

async fn eventually_exits(scratch: &Scratch, words: &[&str], code: i32) -> Verdict {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let verdict = scratch.check(words).await;
        if verdict.code == code {
            return verdict;
        }
        assert!(
            Instant::now() < deadline,
            "check never exited {code}; last {} {}",
            verdict.code,
            verdict.stdout
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A running scratch process with two healthy backends and a token file for operator commands.
struct Farm {
    scratch: Scratch,
    server: ServerProcess,
    token_file: String,
    deep_healthy: Arc<AtomicBool>,
    beta: JoinHandle<()>,
}

impl Farm {
    async fn start() -> Self {
        let scratch = Scratch {
            state: TempDir::new().expect("scratch"),
            management: reserve_tcp_address(),
            traffic: reserve_tcp_address(),
        };
        let deep_healthy = Arc::new(AtomicBool::new(true));
        let (alpha_address, _) = stub(Arc::clone(&deep_healthy)).await;
        let (beta_address, beta) = stub(Arc::clone(&deep_healthy)).await;
        let mut server =
            ServerProcess::start(&scratch.config(&[alpha_address, beta_address]), Some(TOKEN));
        server.wait_for_tcp(scratch.management).await;
        server.wait_for_tcp(scratch.traffic).await;
        let token_file = server
            .token_path
            .clone()
            .expect("token file")
            .display()
            .to_string();
        Self {
            scratch,
            server,
            token_file,
            deep_healthy,
            beta,
        }
    }

    /// An operator command, with the management token of this scratch process.
    async fn operate(&self, words: &[&str]) {
        let endpoint = format!("http://{}", self.scratch.management);
        let mut arguments = ["--endpoint", &endpoint, "--token-file", &self.token_file]
            .map(str::to_owned)
            .to_vec();
        arguments.extend(words.iter().map(|word| (*word).to_owned()));
        let done = run_cli(arguments).await;
        assert_eq!(done.code, 0, "{}", done.stdout);
    }
}

#[tokio::test]
async fn check_exits_ok_warning_and_critical_as_servers_drain_and_die() {
    let farm = Farm::start().await;
    let scratch = &farm.scratch;
    let ok = eventually_exits(scratch, &[], 0).await;
    assert!(
        ok.stdout
            .starts_with("OXIROUTE OK - 1 pools, 2/2 servers available | farm_available=2;2;1 ")
    );

    farm.operate(&["server", "drain", "--pool", "farm", "alpha"])
        .await;
    let warning = eventually_exits(scratch, &[], 1).await;
    assert!(
        warning
            .stdout
            .starts_with("OXIROUTE WARNING - pool farm 1/2 available | ")
    );
    assert_eq!(
        scratch.check(&["--warn-available-below", "1"]).await.code,
        0
    );

    let json = scratch.check(&["--json"]).await;
    let report: Value = serde_json::from_str(&json.stdout).expect("--json prints one JSON object");
    assert_eq!((json.code, report["status"].as_str()), (1, Some("warning")));
    assert_eq!(report["findings"][0]["pool"], "farm");

    let socket = scratch.socket().display().to_string();
    let over_socket = run_cli(vec!["--socket".into(), socket, "check".into()]).await;
    assert_eq!(
        over_socket.code, 1,
        "the read-only socket needs no token: {}",
        over_socket.stdout
    );

    farm.beta.abort();
    let critical = eventually_exits(scratch, &[], 2).await;
    assert!(
        critical
            .stdout
            .starts_with("OXIROUTE CRITICAL - pool farm 0/2 available")
    );

    farm.operate(&["server", "ready", "--pool", "farm", "alpha"])
        .await;
    eventually_exits(scratch, &["--warn-available-below", "1"], 0).await;
    farm.server.shutdown();
}

#[tokio::test]
async fn check_requests_the_canary_url_and_reports_unknown_for_what_it_cannot_judge() {
    let farm = Farm::start().await;
    let scratch = &farm.scratch;
    let url = format!("http://{}/", scratch.traffic);
    eventually_exits(scratch, &[], 0).await;
    assert_eq!(scratch.check(&["--canary", "--url", &url]).await.code, 0);
    let wrong = scratch
        .check(&["--canary", "--url", &url, "--expect-status", "404"])
        .await;
    assert_eq!(wrong.code, 2);
    assert!(
        wrong.stdout.contains("answered 200 instead of 404"),
        "{}",
        wrong.stdout
    );
    let refused = format!("http://{}/", reserve_tcp_address());
    assert_eq!(
        scratch.check(&["--canary", "--url", &refused]).await.code,
        2
    );

    let missing = scratch.check(&["--pool", "nosuch"]).await;
    assert_eq!(missing.code, 3);
    assert!(
        missing
            .stdout
            .starts_with("OXIROUTE UNKNOWN - pool nosuch does not exist")
    );
    let bad_option = scratch.check(&["--no-such-option"]).await;
    assert_eq!(
        bad_option.code, 3,
        "usage errors of check are UNKNOWN, not clap's 2"
    );
    assert_eq!(
        bad_option.stdout,
        "OXIROUTE UNKNOWN - unexpected argument '--no-such-option' found (see `oxiroute check --help`)"
    );
    let unreachable = run_cli(vec![
        "--endpoint".into(),
        format!("http://{}", reserve_tcp_address()),
        "check".into(),
    ])
    .await;
    assert_eq!(unreachable.code, 3);
    assert!(
        unreachable
            .stdout
            .starts_with("OXIROUTE UNKNOWN - status page unavailable")
    );
    farm.server.shutdown();
}

#[tokio::test]
async fn check_judges_the_pool_canary_that_the_status_page_and_metrics_report() {
    let farm = Farm::start().await;
    let scratch = &farm.scratch;
    let first = eventually_exits(scratch, &[], 0).await;
    let seen = |line: &str| line.contains(" farm_canary_ms=");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut line = first.stdout;
    while !seen(&line) {
        assert!(Instant::now() < deadline, "the canary never ran: {line}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        line = scratch.check(&[]).await.stdout;
    }
    let metrics = http_support::http_request(scratch.management, "GET", "/metrics", &[], &[]).await;
    let text = String::from_utf8_lossy(&metrics.body).into_owned();
    assert!(
        text.contains("oxiroute_canary_success{pool=\"farm\"} 1"),
        "{text}"
    );
    assert!(text.contains("oxiroute_canary_last_success_timestamp_seconds{pool=\"farm\"} "));

    farm.deep_healthy.store(false, Ordering::Relaxed);
    let critical = eventually_exits(scratch, &[], 2).await;
    assert!(
        critical
            .stdout
            .contains("pool farm canary failed (unexpected_status)"),
        "{}",
        critical.stdout
    );
    let metrics = http_support::http_request(scratch.management, "GET", "/metrics", &[], &[]).await;
    let text = String::from_utf8_lossy(&metrics.body).into_owned();
    assert!(
        text.contains("oxiroute_canary_success{pool=\"farm\"} 0"),
        "{text}"
    );

    farm.deep_healthy.store(true, Ordering::Relaxed);
    eventually_exits(scratch, &[], 0).await;
    farm.server.shutdown();
}
