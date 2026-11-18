#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
#![cfg(target_os = "linux")]

#[path = "../../support/config.rs"]
mod config_support;
#[path = "../../support/fixtures.rs"]
mod fixture_support;
#[path = "../../support/http.rs"]
mod http_support;
#[path = "../../support/process.rs"]
mod process_support;

mod udp_workers;
mod unix_mode;

use std::{
    future::Future,
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpPathSelector, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpService, HttpVersionPolicy, L4Service, Listener, ListenerBind, Management,
    PoolResilience, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint,
    UpstreamPool,
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    time::sleep,
};

use config_support::empty_config;
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const WAIT: Duration = Duration::from_secs(20);
const SUPERVISED_LAUNCHER: &str = "/usr/lib/oxiroute/oxiroute-worker-launcher";

fn runtime_modes() -> Vec<bool> {
    let launcher = Path::new(SUPERVISED_LAUNCHER).is_file();
    if !launcher {
        eprintln!("the supervised launcher is not installed: only the direct runtime is exercised");
    }
    [false]
        .into_iter()
        .chain(launcher.then_some(true))
        .collect()
}

fn listener(name: &str, bind: ListenerBind) -> Listener {
    Listener {
        name: name.into(),
        bind,
        protocol: Protocol::Http,
        service: Some("fixed".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    }
}

fn fixed_service(body: &str) -> HttpService {
    HttpService {
        drain_deadline_ms: None,
        name: "fixed".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: Some(64 * 1024),
                request_buffering: true,
                ..HttpRoutePolicy::default()
            },
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: body.into(),
                headers: Vec::new(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 1_000,
        max_request_body_bytes: Some(1_024),
        gzip: None,
        access_log: None,
    }
}

fn relay_listener(name: &str, bind: ListenerBind) -> Listener {
    Listener {
        protocol: Protocol::Tcp,
        service: Some("relay".into()),
        ..listener(name, bind)
    }
}

fn relay_services(config: &mut ConfigDraft, echo: SocketAddr) {
    config.upstream_pools = vec![UpstreamPool {
        name: "echo".into(),
        servers: Vec::new(),
        endpoints: vec![UpstreamEndpoint::Socket { address: echo }],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: PoolResilience::default(),
        queue: None,
    }];
    config.l4_services = vec![L4Service {
        drain_deadline_ms: None,
        name: "relay".into(),
        upstream_pool: "echo".into(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 120_000,
        lifetime_timeout_ms: Some(600_000),
        proxy_protocol: None,
        udp: None,
    }];
}

fn managed_config(
    body: &str,
    relay: ListenerBind,
    echo: SocketAddr,
) -> (ConfigDraft, SocketAddr, SocketAddr) {
    let (management, web) = (reserve_tcp_address(), reserve_tcp_address());
    let mut config = empty_config();
    config.management = Some(Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });
    config.listeners = vec![
        relay_listener("relay", relay),
        listener(
            "web",
            ListenerBind::Socket {
                address: web,
                ipv6_only: false,
            },
        ),
    ];
    config.http_services = vec![fixed_service(body)];
    relay_services(&mut config, echo);
    (config, management, web)
}

fn spawn_echo_upstream() -> SocketAddr {
    let upstream =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("echo upstream");
    let address = upstream.local_addr().expect("echo upstream address");
    upstream
        .set_nonblocking(true)
        .expect("nonblocking echo upstream");
    tokio::spawn(async move {
        let upstream = tokio::net::TcpListener::from_std(upstream).expect("tokio echo upstream");
        while let Ok((mut stream, _)) = upstream.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 256];
                while let Ok(count) = stream.read(&mut buffer).await {
                    if count == 0 || stream.write_all(&buffer[..count]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    address
}

async fn echo<S>(stream: &mut S, message: &[u8])
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(WAIT, async {
        stream.write_all(message).await.expect("write to the relay");
        let mut echoed = vec![0_u8; message.len()];
        stream
            .read_exact(&mut echoed)
            .await
            .expect("read from the relay");
        assert_eq!(echoed, message);
    })
    .await
    .expect("relay echo arrives");
}

async fn web_body(address: SocketAddr) -> String {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("web client");
    fixed_body(&mut stream).await
}

struct Instance {
    server: ServerProcess,
    management: SocketAddr,
    web: SocketAddr,
    config: ConfigDraft,
    supervised: bool,
}

impl Instance {
    async fn start(
        (config, management, web): (ConfigDraft, SocketAddr, SocketAddr),
        supervised: bool,
    ) -> Self {
        let validated = config.clone().validate().expect("valid initial config");
        let mut server = if supervised {
            ServerProcess::start_supervised(&validated, TOKEN)
        } else {
            ServerProcess::start(&validated, Some(TOKEN))
        };
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(web).await;
        Self {
            server,
            management,
            web,
            config,
            supervised,
        }
    }

    async fn generation(&self) -> Value {
        let authorization = format!("Bearer {TOKEN}");
        http_request(
            self.management,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .json()["generation"]
            .clone()
    }

    async fn apply(&mut self, edit: impl FnOnce(&mut ConfigDraft)) -> Value {
        let before = self.generation().await["activeRevision"].clone();
        edit(&mut self.config);
        write_config(
            &self.server.config_path,
            &self.config.clone().validate().expect("valid edited config"),
        );
        let deadline = Instant::now() + WAIT;
        loop {
            let generation = self.generation().await;
            if generation["activeRevision"] != before {
                return generation;
            }
            assert!(
                Instant::now() < deadline,
                "the edited revision did not activate: {generation}\n{}",
                self.server.stderr()
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn rejects(&mut self, edit: impl FnOnce(&mut ConfigDraft)) {
        let active = self.generation().await["activeRevision"].clone();
        edit(&mut self.config);
        write_config(
            &self.server.config_path,
            &self
                .config
                .clone()
                .validate()
                .expect("valid rejected config"),
        );
        wait_until("the candidate is turned down", || async {
            let generation = self.generation().await;
            (generation["activeRevision"] == active
                && (generation["lastFailure"] != Value::Null
                    || generation["lastRejection"] != Value::Null))
                .then_some(())
        })
        .await;
    }

    fn stop(self) {
        self.server.shutdown();
    }
}

async fn wait_until<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(25)).await;
    }
}

async fn fixed_body<S>(stream: &mut S) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(WAIT, read_fixed_body(stream))
        .await
        .expect("fixed response arrives")
}

async fn read_fixed_body<S>(stream: &mut S) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write request");
    let mut received = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let read = stream.read(&mut chunk).await.expect("read response");
        assert_ne!(
            read, 0,
            "connection closed before the response: {received:?}"
        );
        received.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&received);
        if let Some((head, body)) = text.split_once("\r\n\r\n")
            && let Some(length) = head
                .to_ascii_lowercase()
                .lines()
                .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
                .and_then(|length| length.trim().parse::<usize>().ok())
            && body.len() >= length
        {
            return body[..length].to_owned();
        }
    }
}
