//! `POST /api/v1/servers/evacuate` and `POST /api/v1/pools/evacuate` against a real server process
//! with localhost origins that the test holds and releases explicitly.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[allow(dead_code)]
#[path = "support/process.rs"]
mod process_support;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, HttpPathSelector, HttpProxyPolicy, HttpRetryBodySafety,
    HttpRetryMethodSafety, HttpRetryPolicy, HttpRoute, HttpRouteAction, HttpService,
    HttpVersionPolicy, Listener, Management, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse,
    UpstreamPool, UpstreamServer,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
    task::JoinHandle,
    time::timeout,
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{HttpResponse, http_request};
use process_support::{ServerProcess, reserve_tcp_address};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const TEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy)]
enum Mode {
    Reply,
    /// Reads the request, then waits for the test to release it.
    Hold,
}

#[derive(Debug, Eq, PartialEq)]
enum Event {
    Arrived(String),
    /// The proxy closed the connection before the origin answered.
    Dropped(String),
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
                tokio::spawn(serve(
                    stream,
                    name.clone(),
                    mode,
                    Arc::clone(&served),
                    events_tx.clone(),
                ));
            }
        });
        Self {
            address,
            events,
            gate,
            task,
        }
    }

    fn release(&self) {
        self.gate.add_permits(1);
    }

    async fn next(&mut self) -> Event {
        timeout(TEST_TIMEOUT, self.events.recv())
            .await
            .expect("origin event in time")
            .expect("origin task alive")
    }

    async fn arrival(&mut self, request_id: &str) {
        assert_eq!(self.next().await, Event::Arrived(request_id.into()));
    }

    async fn dropped(&mut self, request_id: &str) {
        assert_eq!(self.next().await, Event::Dropped(request_id.into()));
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut stream: TcpStream,
    name: String,
    mode: Mode,
    gate: Arc<Semaphore>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut head = Vec::new();
    let mut buffer = [0; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer).await {
            Ok(read) if read > 0 => head.extend_from_slice(&buffer[..read]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let request_id = head
        .lines()
        .find_map(|line| line.strip_prefix("x-request-id:"))
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    events
        .send(Event::Arrived(request_id.clone()))
        .expect("test alive");
    if matches!(mode, Mode::Hold) {
        tokio::select! {
            permit = gate.acquire() => permit.expect("gate open").forget(),
            _ = stream.read(&mut buffer) => {
                events.send(Event::Dropped(request_id)).expect("test alive");
                return;
            }
        }
    }
    let body = format!("from-{name}");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Origin: {name}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn server_name(index: usize) -> String {
    format!(
        "node-{}",
        char::from(b'a' + u8::try_from(index).expect("few servers"))
    )
}

fn pool_config(name: &str, origins: &[Origin]) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: origins
            .iter()
            .enumerate()
            .map(|(index, origin)| UpstreamServer {
                name: server_name(index),
                endpoint: socket_endpoint(origin.address),
                max_connections: None,
                dns_resolution: DnsResolutionPolicy::OnConnect,
            })
            .collect(),
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::First,
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
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

fn route(pool: &str) -> HttpRoute {
    HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix {
            value: format!("/{pool}"),
        },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::Proxy {
            upstream_pool: pool.into(),
            policy: HttpProxyPolicy {
                retry: HttpRetryPolicy {
                    max_retries: 0,
                    method_safety: HttpRetryMethodSafety::GetHead,
                    body_safety: HttpRetryBodySafety::Empty,
                    ..HttpRetryPolicy::default()
                },
                ..HttpProxyPolicy::default()
            }
            .into(),
        },
    }
}

struct Farm {
    management: SocketAddr,
    traffic: SocketAddr,
    pools: Vec<(&'static str, Vec<Origin>)>,
    server: Option<ServerProcess>,
}

impl Farm {
    /// One pool per entry, each with one origin per mode, named `node-a`, `node-b`, and so on.
    async fn start(pools: &[(&'static str, &[Mode])]) -> Self {
        let mut farm = Self {
            management: reserve_tcp_address(),
            traffic: reserve_tcp_address(),
            pools: Vec::new(),
            server: None,
        };
        for (name, modes) in pools {
            let mut origins = Vec::new();
            for (index, mode) in modes.iter().enumerate() {
                origins.push(Origin::start(server_name(index), *mode).await);
            }
            farm.pools.push((name, origins));
        }
        let config = farm.config().validate().expect("valid evacuation config");
        let mut server = ServerProcess::start(&config, Some(TOKEN));
        server.wait_for_tcp(farm.management).await;
        server.wait_for_tcp(farm.traffic).await;
        farm.server = Some(server);
        farm
    }

    fn config(&self) -> ConfigDraft {
        ConfigDraft {
            management: Some(Management {
                bind: self.management,
                ui_dir: None,
                token_file: None,
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
            upstream_pools: self
                .pools
                .iter()
                .map(|(name, origins)| pool_config(name, origins))
                .collect(),
            http_services: vec![HttpService {
                drain_deadline_ms: None,
                name: "traffic".into(),
                routes: self.pools.iter().map(|(name, _)| route(name)).collect(),
                automatic_response_headers: false,
                upstream_io_timeout_ms: 30_000,
                max_request_body_bytes: None,
                gzip: None,
                access_log: None,
            }],
            ..empty_config()
        }
    }

    fn origin(&mut self, pool: &str, server: usize) -> &mut Origin {
        let (_, origins) = self
            .pools
            .iter_mut()
            .find(|(name, _)| *name == pool)
            .expect("known pool");
        &mut origins[server]
    }

    fn get(&self, pool: &str, request_id: &str) -> JoinHandle<HttpResponse> {
        let address = self.traffic;
        let path = format!("/{pool}");
        let request_id = request_id.to_owned();
        tokio::spawn(async move {
            http_request(address, "GET", &path, &[("X-Request-Id", &request_id)], b"").await
        })
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &Value,
    ) -> HttpResponse {
        let authorization = format!("Bearer {}", token.unwrap_or_default());
        let mut headers = vec![("Content-Type", "application/json")];
        if token.is_some() {
            headers.push(("Authorization", &authorization));
        }
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).expect("body")
        };
        http_request(self.management, method, path, &headers, &body).await
    }

    async fn get_json(&self, path: &str) -> Value {
        let response = self.call("GET", path, Some(TOKEN), &Value::Null).await;
        assert_eq!(response.status, 200, "{}", response.text());
        response.json()
    }

    async fn revision(&self) -> String {
        self.get_json("/api/v1/generations").await["generation"]["activeRevision"]
            .as_str()
            .expect("active revision")
            .to_owned()
    }

    /// Posts `body` plus the current `expectedActiveRevision` to a management route.
    async fn post(&self, path: &str, mut body: Value) -> HttpResponse {
        body["expectedActiveRevision"] = json!(self.revision().await);
        self.call("POST", path, Some(TOKEN), &body).await
    }

    async fn evacuate_servers(&self, targets: &[(&str, &str)], force: bool) -> HttpResponse {
        let targets = targets
            .iter()
            .map(|(pool, server)| json!({ "pool": pool, "server": server }))
            .collect::<Vec<_>>();
        self.post(
            "/api/v1/servers/evacuate",
            json!({ "targets": targets, "force": force }),
        )
        .await
    }

    async fn evacuate_pools(&self, pools: &[&str], force: bool) -> HttpResponse {
        self.post(
            "/api/v1/pools/evacuate",
            json!({ "pools": pools, "force": force }),
        )
        .await
    }

    /// The administrative state of every `(pool, server)` pair, in the order of the inventory.
    async fn states(&self) -> Vec<(String, String, String)> {
        self.get_json("/api/v1/servers").await["servers"]
            .as_array()
            .expect("servers")
            .iter()
            .map(|entry| {
                let field = |value: &Value| value.as_str().expect("string").to_owned();
                (
                    field(&entry["pool"]),
                    field(&entry["server"]["name"]),
                    field(&entry["server"]["administrativeState"]),
                )
            })
            .collect()
    }

    async fn drained(&self) -> Vec<String> {
        self.states()
            .await
            .into_iter()
            .filter(|(_, _, state)| state == "drain")
            .map(|(pool, server, _)| format!("{pool}/{server}"))
            .collect()
    }

    async fn events(&self) -> Vec<Value> {
        self.get_json("/api/v2/events?after=0&limit=1000").await["events"]
            .as_array()
            .expect("events")
            .iter()
            .filter(|event| event["event"] == "server_evacuation")
            .cloned()
            .collect()
    }

    async fn audit(&self, operation: &str) -> Vec<Value> {
        self.get_json("/api/v1/audit?limit=1000").await["records"]
            .as_array()
            .expect("audit records")
            .iter()
            .filter(|record| record["operation"] == operation)
            .cloned()
            .collect()
    }

    async fn cli(&self, words: &[&str]) -> std::process::Output {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_oxiroute"));
        command
            .args(["--endpoint", &format!("http://{}", self.management)])
            .arg("--token-file")
            .arg(self.token_path())
            .args(["--output", "json"])
            .args(words);
        tokio::task::spawn_blocking(move || command.output().expect("oxiroute CLI"))
            .await
            .expect("CLI task")
    }

    fn token_path(&self) -> std::path::PathBuf {
        let server = self.server.as_ref().expect("running server");
        server.token_path.clone().expect("management token file")
    }

    async fn metrics(&self) -> String {
        let response = http_request(self.management, "GET", "/metrics", &[], &[]).await;
        assert_eq!(response.status, 200);
        String::from_utf8(response.body).expect("metrics UTF-8")
    }

    fn shutdown(mut self) {
        self.server.take().expect("running server").shutdown();
    }
}

fn assert_report(response: &HttpResponse, force: bool, moved: u64, forced: u64, left: &Value) {
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(
        response.json(),
        json!({
            "outcome": "evacuated",
            "force": force,
            "cancelledAndRedispatched": moved,
            "forced": forced,
            "leftRunning": left,
        })
    );
}

#[tokio::test]
async fn evacuating_a_server_moves_its_held_request_and_leaves_it_drained() {
    let mut farm = Farm::start(&[("render", &[Mode::Hold, Mode::Reply])]).await;
    let client = farm.get("render", "moved-get");
    farm.origin("render", 0).arrival("moved-get").await;

    let response = farm.evacuate_servers(&[("render", "node-a")], false).await;

    assert_report(&response, false, 1, 0, &json!({}));
    farm.origin("render", 0).dropped("moved-get").await;
    let served = client.await.expect("client task");
    assert_eq!(served.status, 200, "{}", served.text());
    assert_eq!(served.header("x-origin"), Some("node-b"));
    assert_eq!(served.header("x-request-id"), Some("moved-get"));
    assert_eq!(farm.drained().await, ["render/node-a"]);
    let metrics = farm.metrics().await;
    assert!(
        metrics.contains(
            "oxiroute_evacuated_requests_total{pool=\"render\",server=\"node-a\",result=\"redispatched\"} 1"
        ),
        "{metrics}"
    );
    farm.shutdown();
}

#[tokio::test]
async fn an_idle_server_is_drained_and_reports_nothing_moved() {
    let farm = Farm::start(&[("render", &[Mode::Reply, Mode::Reply])]).await;

    let response = farm.evacuate_servers(&[("render", "node-a")], false).await;

    assert_report(&response, false, 0, 0, &json!({}));
    assert_eq!(farm.drained().await, ["render/node-a"]);
    farm.shutdown();
}

#[tokio::test]
async fn the_same_server_name_in_two_pools_is_evacuated_in_both() {
    let farm = Farm::start(&[
        ("render", &[Mode::Reply, Mode::Reply]),
        ("voice", &[Mode::Reply, Mode::Reply]),
    ])
    .await;

    let response = farm
        .evacuate_servers(&[("render", "node-a"), ("voice", "node-a")], false)
        .await;

    assert_report(&response, false, 0, 0, &json!({}));
    assert_eq!(farm.drained().await, ["render/node-a", "voice/node-a"]);
    farm.shutdown();
}

#[tokio::test]
async fn an_unknown_pool_or_server_rejects_the_whole_batch() {
    let farm = Farm::start(&[("render", &[Mode::Reply, Mode::Reply])]).await;

    let pool = farm
        .evacuate_servers(&[("render", "node-a"), ("missing", "node-a")], false)
        .await;
    let server = farm
        .evacuate_servers(&[("render", "node-a"), ("render", "missing")], false)
        .await;
    let pools = farm.evacuate_pools(&["render", "missing"], false).await;

    assert_eq!(pool.status, 404);
    assert_eq!(pool.json()["error"]["code"], "pool_not_found");
    assert_eq!(server.status, 404);
    assert_eq!(server.json()["error"]["code"], "server_not_found");
    assert_eq!(pools.status, 404);
    assert_eq!(pools.json()["error"]["code"], "pool_not_found");
    assert!(farm.drained().await.is_empty());
    assert!(farm.events().await.is_empty());
    farm.shutdown();
}

#[tokio::test]
async fn bad_requests_are_refused_without_changing_anything() {
    let farm = Farm::start(&[("render", &[Mode::Reply, Mode::Reply])]).await;
    let revision = farm.revision().await;
    let target = json!([{ "pool": "render", "server": "node-a" }]);

    let stale = farm
        .call(
            "POST",
            "/api/v1/servers/evacuate",
            Some(TOKEN),
            &json!({ "targets": target, "expectedActiveRevision": "f".repeat(64) }),
        )
        .await;
    let missing_revision = farm
        .call(
            "POST",
            "/api/v1/servers/evacuate",
            Some(TOKEN),
            &json!({ "targets": target }),
        )
        .await;
    let unknown_field = farm
        .call(
            "POST",
            "/api/v1/pools/evacuate",
            Some(TOKEN),
            &json!({ "pools": ["render"], "expectedActiveRevision": revision, "state": "drain" }),
        )
        .await;
    let empty = farm.evacuate_servers(&[], false).await;
    let empty_pools = farm.evacuate_pools(&[], false).await;
    let wrong_method = farm
        .call("GET", "/api/v1/servers/evacuate", Some(TOKEN), &Value::Null)
        .await;

    assert_eq!(
        (stale.status, stale.json()["error"]["code"].as_str()),
        (409, Some("generation_conflict"))
    );
    assert_eq!(missing_revision.status, 400);
    assert_eq!(unknown_field.status, 400);
    assert_eq!(empty.status, 400);
    assert_eq!(empty_pools.status, 400);
    assert_eq!(empty.json()["error"]["code"], "invalid_batch");
    assert_eq!(wrong_method.status, 405);
    assert_eq!(wrong_method.header("allow"), Some("POST"));
    assert!(farm.drained().await.is_empty());
    farm.shutdown();
}

#[tokio::test]
async fn evacuation_needs_the_management_token() {
    let farm = Farm::start(&[("render", &[Mode::Reply])]).await;
    let revision = farm.revision().await;
    let servers = json!({
        "targets": [{ "pool": "render", "server": "node-a" }],
        "expectedActiveRevision": revision,
    });
    let pools = json!({ "pools": ["render"], "expectedActiveRevision": revision });

    for (path, body) in [
        ("/api/v1/servers/evacuate", servers),
        ("/api/v1/pools/evacuate", pools),
    ] {
        assert_eq!(farm.call("POST", path, None, &body).await.status, 401);
        assert_eq!(
            farm.call("POST", path, Some("wrong"), &body).await.status,
            401
        );
    }
    assert!(farm.drained().await.is_empty());
    farm.shutdown();
}

#[tokio::test]
async fn pool_evacuation_drains_every_server_and_leaves_requests_with_nowhere_to_go() {
    let mut farm = Farm::start(&[("render", &[Mode::Hold, Mode::Reply])]).await;
    let client = farm.get("render", "stays");
    farm.origin("render", 0).arrival("stays").await;

    let response = farm.evacuate_pools(&["render"], false).await;

    assert_report(&response, false, 0, 0, &json!({ "no_destination": 1 }));
    assert_eq!(farm.drained().await, ["render/node-a", "render/node-b"]);
    farm.origin("render", 0).release();
    let served = client.await.expect("client task");
    assert_eq!(
        served.header("x-origin"),
        Some("node-a"),
        "{}",
        served.text()
    );
    farm.shutdown();
}

#[tokio::test]
async fn forced_pool_evacuation_cuts_the_request_with_the_evacuated_reason() {
    let mut farm = Farm::start(&[("render", &[Mode::Hold, Mode::Reply])]).await;
    let client = farm.get("render", "cut");
    farm.origin("render", 0).arrival("cut").await;

    let response = farm.evacuate_pools(&["render"], true).await;

    assert_report(&response, true, 0, 1, &json!({}));
    let cut = client.await.expect("client task");
    assert_eq!(cut.status, 503, "{}", cut.text());
    assert_eq!(cut.header("x-oxiroute-reason"), Some("evacuated"));
    farm.origin("render", 0).dropped("cut").await;
    farm.shutdown();
}

#[tokio::test]
async fn evacuations_are_audited_and_published_as_events() {
    let mut farm = Farm::start(&[
        ("render", &[Mode::Hold, Mode::Reply]),
        ("voice", &[Mode::Reply, Mode::Reply]),
    ])
    .await;
    let client = farm.get("render", "event-get");
    farm.origin("render", 0).arrival("event-get").await;
    farm.evacuate_servers(&[("render", "node-a"), ("voice", "node-a")], false)
        .await;
    farm.evacuate_pools(&["voice"], false).await;
    client.await.expect("client task");

    let audited = farm.audit("server_evacuate").await;
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0]["category"], "control");
    assert_eq!(audited[0]["result"], "succeeded");
    assert_eq!(farm.audit("pool_evacuate").await.len(), 1);
    let events = farm.events().await;
    let outcomes = events
        .iter()
        .map(|event| &event["outcome"])
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        [
            &json!({ "type": "evacuated", "pool": "render", "server": "node-a", "force": false,
                     "cancelledAndRedispatched": 1, "forced": 0, "leftRunning": {} }),
            &json!({ "type": "evacuated", "pool": "voice", "server": "node-a", "force": false,
                     "cancelledAndRedispatched": 0, "forced": 0, "leftRunning": {} }),
            &json!({ "type": "evacuated", "pool": "voice", "force": false,
                     "cancelledAndRedispatched": 0, "forced": 0, "leftRunning": {} }),
        ]
    );
    let v1 = farm.get_json("/api/v1/events?after=0&limit=1000").await;
    assert!(
        v1["events"]
            .as_array()
            .expect("events")
            .iter()
            .all(|event| event["event"] != "server_evacuation")
    );
    farm.shutdown();
}

#[tokio::test]
async fn the_cli_evacuates_a_server_and_a_pool_against_the_real_api() {
    let mut farm = Farm::start(&[
        ("render", &[Mode::Hold, Mode::Reply]),
        ("voice", &[Mode::Hold, Mode::Reply]),
    ])
    .await;
    let moved = farm.get("render", "cli-moved");
    farm.origin("render", 0).arrival("cli-moved").await;

    let server = farm
        .cli(&["server", "evacuate", "--pool", "render", "node-a"])
        .await;

    assert!(
        server.status.success(),
        "{}",
        String::from_utf8_lossy(&server.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&server.stdout).expect("report JSON"),
        json!({
            "outcome": "evacuated", "force": false, "cancelledAndRedispatched": 1,
            "forced": 0, "leftRunning": {},
        })
    );
    assert_eq!(
        moved.await.expect("client task").header("x-origin"),
        Some("node-b")
    );

    let cut = farm.get("voice", "cli-cut");
    farm.origin("voice", 0).arrival("cli-cut").await;
    let pool = farm.cli(&["pool", "evacuate", "--force", "voice"]).await;

    assert!(
        pool.status.success(),
        "{}",
        String::from_utf8_lossy(&pool.stderr)
    );
    let report: Value = serde_json::from_slice(&pool.stdout).expect("report JSON");
    assert_eq!(
        (report["force"].clone(), report["forced"].clone()),
        (json!(true), json!(1))
    );
    assert_eq!(cut.await.expect("client task").status, 503);
    assert_eq!(
        farm.drained().await,
        ["render/node-a", "voice/node-a", "voice/node-b"]
    );

    let missing = farm.cli(&["pool", "evacuate", "missing"]).await;
    assert!(!missing.status.success());
    farm.shutdown();
}
