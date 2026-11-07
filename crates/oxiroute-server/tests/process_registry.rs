#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{
    collections::BTreeSet,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, HealthCheck, HealthCheckType, HealthStartup,
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy,
    Listener, Management, PoolRegistration, Protocol, Registry, RegistryEndpoint, RegistryRole,
    UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use oxiroute_server::registry::auth::mint_token;
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
        let (voice_secret, line) =
            mint_token("voice-agent", "register pools=voice").expect("token");
        let tokens = state.path().join("registry.tokens");
        fs::write(&tokens, format!("{line}\n")).expect("tokens");
        fs::set_permissions(&tokens, fs::Permissions::from_mode(0o600)).expect("mode");
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
        let registration = PoolRegistration {
            allowed_cidrs: vec!["127.0.0.0/8".into()],
            require_peer_address: false,
            min_ttl_ms: 2_000,
            default_ttl_ms: 3_000,
            ..PoolRegistration::default()
        };
        let pool = |name: &str, static_server: Option<SocketAddr>| UpstreamPool {
            name: name.into(),
            servers: static_server
                .map(|address| UpstreamServer {
                    name: "static".into(),
                    endpoint: UpstreamEndpoint::Socket { address },
                    max_connections: None,
                    dns_resolution: DnsResolutionPolicy::OnConnect,
                })
                .into_iter()
                .collect(),
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: Some(HealthCheck {
                kind: HealthCheckType::Tcp,
                interval_ms: 1_000,
                timeout_ms: 500,
                healthy_threshold: 1,
                unhealthy_threshold: 1,
                startup: HealthStartup::Healthy,
                fast_interval_ms: None,
                down_interval_ms: None,
                host: None,
                path: None,
                expected_status: None,
                http_version: None,
            }),
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
            registration: Some(registration.clone()),
            resilience: oxiroute_config::PoolResilience::default(),
        };
        let route = |path: &str, pool: &str| HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: path.into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::Proxy {
                upstream_pool: pool.into(),
                policy: HttpProxyPolicy::default(),
            },
        };
        ConfigDraft {
            max_connections: extra_root_limit,
            management: Some(Management {
                bind: self.management,
                ui_dir: None,
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
            }],
            upstream_pools: vec![pool("render", None), pool("voice", None)],
            http_services: vec![HttpService {
                name: "traffic".into(),
                routes: vec![route("/voice", "voice"), route("/", "render")],
                automatic_response_headers: true,
                upstream_io_timeout_ms: 2_000,
                max_request_body_bytes: Some(64 * 1024),
                gzip: None,
                access_log: None,
            }],
            ..empty_config()
        }
        .validate()
        .expect("valid registry config")
    }

    async fn start(&self) -> ServerProcess {
        let mut server = ServerProcess::start(&self.config(None), Some(TOKEN));
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

async fn eventually(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn registered_servers_take_traffic_and_leave_by_lease_or_deregistration() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (alpha, _a) = stub("alpha").await;
    let (beta, _b) = stub("beta").await;
    let (gamma, _g) = stub("gamma").await;
    assert_eq!(farm.register("render", "alpha", alpha, 2_000).await, 201);
    assert_eq!(farm.register("render", "beta", beta, 2_000).await, 201);
    assert_eq!(farm.register("render", "gamma", gamma, 60_000).await, 201);
    assert_eq!(farm.register("render", "gamma", gamma, 60_000).await, 200);

    assert_eq!(
        farm.served_by("/", 12).await,
        ["alpha", "beta", "gamma"].map(String::from).into()
    );

    // alpha keeps heartbeating, beta stops: only beta must leave within its lease.
    eventually("beta to expire while alpha stays", async || {
        assert_eq!(farm.beat("render", "alpha").await, 200);
        !farm.served_by("/", 9).await.contains("beta")
    })
    .await;
    assert!(farm.served_by("/", 9).await.contains("alpha"));
    let (status, _) = farm
        .api(
            "DELETE",
            "/api/v1/pools/render/servers/gamma",
            TOKEN,
            &Value::Null,
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(
        farm.served_by("/", 9).await,
        ["alpha"].map(String::from).into()
    );
    assert_eq!(
        farm.beat("render", "beta").await,
        404,
        "an expired worker is told to register again"
    );
    server.shutdown();
}

#[tokio::test]
async fn a_second_pool_registers_with_the_same_model_and_a_scoped_token() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let (render, _r) = stub("gpu-1").await;
    let body = json!({"address": voice.to_string()});

    let (status, _) = farm
        .api(
            "PUT",
            "/api/v1/pools/voice/servers/tts-1",
            &farm.voice_secret,
            &body,
        )
        .await;
    assert_eq!(status, 201);
    let (denied, error) = farm
        .api(
            "PUT",
            "/api/v1/pools/render/servers/tts-1",
            &farm.voice_secret,
            &body,
        )
        .await;
    assert_eq!(
        (denied, error["error"]["code"].as_str()),
        (403, Some("scope_denied"))
    );
    let (listing, _) = farm
        .api(
            "GET",
            "/api/v1/registrations",
            &farm.voice_secret,
            &Value::Null,
        )
        .await;
    assert_eq!(listing, 403, "a register-only token cannot read");
    let (nobody, _) = farm
        .api(
            "PUT",
            "/api/v1/pools/voice/servers/tts-2",
            "not-a-token",
            &body,
        )
        .await;
    assert_eq!(nobody, 401);
    assert_eq!(farm.register("render", "gpu-1", render, 60_000).await, 201);

    assert_eq!(
        farm.served_by("/voice/x", 4).await,
        ["tts-1"].map(String::from).into()
    );
    assert_eq!(
        farm.served_by("/", 4).await,
        ["gpu-1"].map(String::from).into()
    );
    let (status, listing) = farm
        .api("GET", "/api/v1/pools/voice/servers", TOKEN, &Value::Null)
        .await;
    assert_eq!(
        (status, listing["servers"][0]["origin"].as_str()),
        (200, Some("dynamic"))
    );
    server.shutdown();
}

#[tokio::test]
async fn registrations_and_operator_drain_survive_generation_reload_and_restart() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (alpha, _a) = stub("alpha").await;
    let (beta, _b) = stub("beta").await;
    farm.register("render", "alpha", alpha, 60_000).await;
    farm.register("render", "beta", beta, 60_000).await;
    let (status, _) = farm
        .api(
            "POST",
            "/api/v1/pools/render/servers/beta/state",
            TOKEN,
            &json!({"state": "drain"}),
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into()
    );

    write_config(&server.config_path, &farm.config(Some(500)));
    eventually("the changed file to become a new generation", async || {
        let metrics = http_request(farm.management, "GET", "/metrics", &[], &[]).await;
        String::from_utf8_lossy(&metrics.body).contains("oxiroute_generation_activations_total 2")
    })
    .await;
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into(),
        "drain survives the reload"
    );
    assert_eq!(
        farm.beat("render", "beta").await,
        200,
        "the lease survives the reload"
    );

    server.shutdown_gracefully();
    let restarted = farm.start().await;
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into(),
        "drain survives a restart"
    );
    let (_, listing) = farm
        .api("GET", "/api/v1/registrations", TOKEN, &Value::Null)
        .await;
    assert_eq!(listing["registrations"].as_array().map(Vec::len), Some(2));
    restarted.shutdown();
}

#[tokio::test]
async fn group_sockets_split_status_from_registration_without_tokens() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let body = json!({"address": voice.to_string()});
    for role in ["read", "register"] {
        let mode = fs::metadata(farm.socket(role))
            .expect("socket")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o660, "{role} socket mode");
    }

    let (status, _) = farm
        .over_socket(
            "register",
            "PUT",
            "/api/v1/pools/voice/servers/tts-1",
            &body,
        )
        .await;
    assert_eq!(status, 201, "a socket client needs no token");
    let (status, error) = farm
        .over_socket(
            "register",
            "PUT",
            "/api/v1/pools/render/servers/tts-1",
            &body,
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (403, Some("scope_denied"))
    );
    let (status, _) = farm
        .over_socket("register", "GET", "/api/v1/registrations", &Value::Null)
        .await;
    assert_eq!(status, 403, "the register socket cannot read");
    let (status, _) = farm
        .over_socket("register", "GET", "/api/v1/status", &Value::Null)
        .await;
    assert_eq!(status, 404, "only registry routes are served");

    let (status, listing) = farm
        .over_socket("read", "GET", "/api/v1/registrations", &Value::Null)
        .await;
    assert_eq!(status, 200);
    assert_eq!(listing["registrations"][0]["registration"]["name"], "tts-1");
    let (status, _) = farm
        .over_socket(
            "read",
            "DELETE",
            "/api/v1/pools/voice/servers/tts-1",
            &Value::Null,
        )
        .await;
    assert_eq!(status, 403, "the read socket cannot change anything");
    let (status, _) = farm
        .over_socket("read", "GET", "/ready", &Value::Null)
        .await;
    assert_eq!(status, 200);
    server.shutdown();
}

#[tokio::test]
async fn the_lan_endpoint_needs_a_scoped_token_and_a_peer_in_range() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let body = json!({"address": voice.to_string()});
    let path = "/api/v1/pools/voice/servers/tts-1";

    assert_eq!(farm.over_lan("PUT", path, "", &body).await.0, 401);
    assert_eq!(
        farm.over_lan("PUT", path, TOKEN, &body).await.0,
        401,
        "the root token is not accepted here"
    );
    assert_eq!(
        farm.over_lan("PUT", path, &farm.voice_secret, &body)
            .await
            .0,
        201
    );
    assert_eq!(
        farm.over_lan(
            "POST",
            &format!("{path}/heartbeat"),
            &farm.voice_secret,
            &Value::Null
        )
        .await
        .0,
        200
    );
    let (status, _) = farm
        .over_lan(
            "GET",
            "/api/v1/registrations",
            &farm.voice_secret,
            &Value::Null,
        )
        .await;
    assert_eq!(status, 403);
    assert_eq!(
        farm.over_lan(
            "PUT",
            "/api/v1/pools/render/servers/x",
            &farm.voice_secret,
            &body
        )
        .await
        .0,
        403
    );
    server.shutdown();

    let mut outside = Farm::new();
    outside.lan_cidrs = vec!["192.0.2.0/8".into()];
    let server = outside.start().await;
    let (status, error) = outside
        .over_lan("PUT", path, &outside.voice_secret, &body)
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (403, Some("peer_not_allowed"))
    );
    server.shutdown();
}
