#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[allow(dead_code)]
#[path = "../support/process.rs"]
mod process_support;

use std::{
    collections::BTreeSet,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, HealthCheck, HealthCheckType, HealthStartup, HttpPathSelector, HttpProxyPolicy,
    HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy, Listener, Management,
    PoolRegistration, Protocol, Registry, RegistryEndpoint, RegistryRole, UpstreamAlgorithm,
    UpstreamPool,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, UnixStream},
    task::JoinHandle,
};

use config_support::{empty_config, socket_bind};
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};

mod helpers;
mod scenarios;

#[allow(clippy::wildcard_imports)]
use helpers::*;

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

struct Farm {
    state: TempDir,
    management: SocketAddr,
    traffic: SocketAddr,
    lan: SocketAddr,
    lan_cidrs: Vec<String>,
    tokens: std::path::PathBuf,
    voice_secret: String,
}

impl Farm {
    fn new() -> Self {
        let state = TempDir::new().expect("registry scratch");
        let tokens = state.path().join("registry.tokens");
        let minted = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .args([
                "registry",
                "token",
                "create",
                "--id",
                "voice-agent",
                "--pool",
                "voice",
            ])
            .arg("--tokens-file")
            .arg(&tokens)
            .output()
            .expect("oxiroute registry token create");
        assert!(
            minted.status.success(),
            "{}",
            String::from_utf8_lossy(&minted.stderr)
        );
        let voice_secret = String::from_utf8(minted.stdout)
            .expect("secret")
            .trim()
            .to_owned();
        Self {
            state,
            management: reserve_tcp_address(),
            traffic: reserve_tcp_address(),
            lan: reserve_tcp_address(),
            lan_cidrs: vec!["127.0.0.0/8".into()],
            tokens,
            voice_secret,
        }
    }

    fn socket(&self, role: &str) -> std::path::PathBuf {
        self.state.path().join(format!("{role}.sock"))
    }

    fn config(&self, extra_root_limit: Option<u64>) -> oxiroute_config::ValidatedConfig {
        ConfigDraft {
            max_connections: extra_root_limit,
            management: Some(Management {
                bind: self.management,
                ui_dir: None,
                token_file: None,
            }),
            registry: Some(Registry {
                state_dir: self.state.path().join("state"),
                tokens_file: Some(self.tokens.clone()),
                endpoints: vec![
                    RegistryEndpoint::Unix {
                        path: self.socket("read"),
                        mode: 0o660,
                        role: RegistryRole::Read,
                        pools: Vec::new(),
                    },
                    RegistryEndpoint::Unix {
                        path: self.socket("register"),
                        mode: 0o660,
                        role: RegistryRole::Register,
                        pools: vec!["voice".into()],
                    },
                    RegistryEndpoint::Tcp {
                        bind: self.test,
                        allow_cidrs: self.lan_cidrs.clone(),
                    },
                ],
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
                h2_initial_stream_window: None,
                h2_initial_connection_window: None,
            }],
            upstream_pools: vec![registered_pool("render"), registered_pool("voice")],
            http_services: vec![HttpService {
                name: "traffic".into(),
                routes: vec![route("/voice", "voice"), route("/", "render")],
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
        .expect("valid registry config")
    }

    async fn start(&self) -> ServerProcess {
        self.ready(ServerProcess::start(&self.config(None), Some(TOKEN)))
            .await
    }

    async fn start_supervised(&self) -> ServerProcess {
        self.ready(ServerProcess::start_supervised(&self.config(None), TOKEN))
            .await
    }

    async fn ready(&self, mut server: ServerProcess) -> ServerProcess {
        server.wait_for_tcp(self.management).await;
        server.wait_for_tcp(self.traffic).await;
        server.wait_for_tcp(self.test).await;
        eventually("the registry sockets", async || {
            self.socket("read").exists() && self.socket("register").exists()
        })
        .await;
        server
    }

    async fn over_socket(
        &self,
        role: &str,
        method: &str,
        path: &str,
        body: &Value,
    ) -> (u16, Value) {
        let bytes = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).expect("body")
        };
        let mut stream = UnixStream::connect(self.socket(role))
            .await
            .expect("socket");
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: registry\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            bytes.len()
        );
        stream
            .write_all(head.as_bytes())
            .await
            .expect("request head");
        stream.write_all(&bytes).await.expect("request body");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.expect("response");
        let response = http_support::HttpResponse::parse(raw);
        let body = if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        };
        (response.status, body)
    }

    async fn cli(&self, endpoint: (&str, &str), words: &[&str]) -> Output {
        let (flag, value) = endpoint;
        let mut arguments = vec![
            flag.to_owned(),
            value.to_owned(),
            "--output".into(),
            "json".into(),
        ];
        arguments.extend(words.iter().map(|word| (*word).to_owned()));
        tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_oxiroute"))
                .args(arguments)
                .output()
                .expect("oxiroute CLI")
        })
        .await
        .expect("CLI task")
    }

    fn socket_arg(&self, role: &str) -> (&'static str, String) {
        ("--socket", self.socket(role).display().to_string())
    }

    async fn over_lan(&self, method: &str, path: &str, token: &str, body: &Value) -> (u16, Value) {
        let bytes = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).expect("body")
        };
        let authorization = format!("Bearer {token}");
        let headers = [("Authorization", authorization.as_str())];
        let headers = if token.is_empty() {
            &headers[..0]
        } else {
            &headers[..]
        };
        let response = http_request(self.test, method, path, headers, &bytes).await;
        let body = if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        };
        (response.status, body)
    }

    async fn api(&self, method: &str, path: &str, token: &str, body: &Value) -> (u16, Value) {
        let bytes = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).expect("body")
        };
        let authorization = format!("Bearer {token}");
        let response = http_request(
            self.management,
            method,
            path,
            &[
                ("Authorization", authorization.as_str()),
                ("Content-Type", "application/json"),
            ],
            &bytes,
        )
        .await;
        let body = if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        };
        (response.status, body)
    }

    async fn register(&self, pool: &str, name: &str, stub: SocketAddr, ttl_ms: u64) -> u16 {
        let body = json!({"address": stub.to_string(), "ttlMs": ttl_ms});
        self.api(
            "PUT",
            &format!("/api/v1/pools/{pool}/servers/{name}"),
            TOKEN,
            &body,
        )
        .await
        .0
    }

    async fn beat(&self, pool: &str, name: &str) -> u16 {
        self.api(
            "POST",
            &format!("/api/v1/pools/{pool}/servers/{name}/heartbeat"),
            TOKEN,
            &Value::Null,
        )
        .await
        .0
    }

    async fn served_by(&self, path: &str, requests: usize) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        for _ in 0..requests {
            let response = http_request(self.traffic, "GET", path, &[], &[]).await;
            if response.status == 200 {
                seen.insert(String::from_utf8(response.body).expect("stub body"));
            }
        }
        seen
    }
}

/// A throwaway backend that answers every request with its own name.
async fn stub(name: &'static str) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("stub bind");
    let address = listener.local_addr().expect("stub address");
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (address, task)
}
