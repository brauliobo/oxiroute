//! `/api/v1/clients` and `/api/v1/connections` against a real scratch process: real listeners, a real origin and real
//! connections, on whichever HTTP data plane the suite runs on.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[allow(dead_code)]
#[path = "../support/process.rs"]
mod process_support;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use oxiroute_config::{
    ClientTracking, ConfigDraft, DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy,
    HttpRoute, HttpRouteAction, HttpService, Listener, Management, Protocol, Stats, StatsPage,
    StatsPageAdminPolicy, UpstreamPool,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpSocket, TcpStream},
    sync::Notify,
    time::{Instant, sleep},
};

use config_support::{empty_config, socket_bind};
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const CLIENT_IP: &str = "127.0.0.2";
const CHUNK: usize = 65_536;
const WAIT: Duration = Duration::from_secs(15);

async fn origin(gate: Arc<Notify>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().expect("origin address");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                let mut head = [0_u8; 4096];
                let Ok(read) = stream.read(&mut head).await else {
                    return;
                };
                if head[..read].starts_with(b"GET /socket") {
                    let reply = "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let mut echo = [0_u8; 256];
                    while let Ok(read) = stream.read(&mut echo).await {
                        if read == 0 || stream.write_all(&echo[..read]).await.is_err() {
                            break;
                        }
                    }
                    return;
                }
                if head[..read].starts_with(b"GET /slow") {
                    let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", 2 * CHUNK);
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.write_all(&vec![b'a'; CHUNK]).await;
                    gate.notified().await;
                    let _ = stream.write_all(&vec![b'b'; CHUNK]).await;
                } else {
                    let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
                    let _ = stream.write_all(reply.as_bytes()).await;
                }
                let _ = stream.shutdown().await;
            });
        }
    });
    address
}

struct Setup {
    management: SocketAddr,
    traffic: SocketAddr,
    page: SocketAddr,
    gate: Arc<Notify>,
    server: ServerProcess,
}

fn config(
    (management, traffic, page): (SocketAddr, SocketAddr, SocketAddr),
    origin: SocketAddr,
    tracking: Option<ClientTracking>,
) -> ConfigDraft {
    let pool: UpstreamPool = serde_json::from_value(json!({
        "name": "farm",
        "servers": [{
            "name": "alpha",
            "endpoint": { "type": "socket", "address": origin.to_string() },
            "max_connections": 8
        }]
    }))
    .expect("farm pool");
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
            bind: management,
            ui_dir: None,
            token_file: None,
        }),
        stats: Some(Stats {
            binds: Vec::new(),
            admin_token_file: None,
            pages: vec![StatsPage {
                bind: page,
                uri_prefix: "/haproxy".into(),
                refresh_ms: 10_000,
                admin: StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
        }),
        client_tracking: tracking,
        listeners: vec![Listener {
            name: "traffic".into(),
            bind: socket_bind(traffic),
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(100),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        upstream_pools: vec![pool],
        http_services: vec![HttpService {
            name: "web".into(),
            routes: vec![route],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 5_000,
            max_request_body_bytes: Some(64 * 1024),
            gzip: None,
            access_log: None,
            drain_deadline_ms: None,
        }],
        ..empty_config()
    }
}

impl Setup {
    async fn start(tracking: Option<ClientTracking>) -> Self {
        let addresses = (
            reserve_tcp_address(),
            reserve_tcp_address(),
            reserve_tcp_address(),
        );
        let gate = Arc::new(Notify::new());
        let validated = config(addresses, origin(Arc::clone(&gate)).await, tracking)
            .validate()
            .expect("valid client tracking config");
        let mut server = ServerProcess::start(&validated, Some(TOKEN));
        for address in [addresses.0, addresses.1, addresses.2] {
            server.wait_for_tcp(address).await;
        }
        Self {
            management: addresses.0,
            traffic: addresses.1,
            page: addresses.2,
            gate,
            server,
        }
    }

    async fn client(&self) -> TcpStream {
        let socket = TcpSocket::new_v4().expect("client socket");
        socket
            .bind(format!("{CLIENT_IP}:0").parse().expect("client address"))
            .expect("bind the client address");
        socket
            .connect(self.traffic)
            .await
            .expect("traffic connection")
    }

    async fn api(&self, path: &str) -> http_support::HttpResponse {
        let bearer = format!("Bearer {TOKEN}");
        http_request(
            self.management,
            "GET",
            path,
            &[("Authorization", bearer.as_str())],
            &[],
        )
        .await
    }

    async fn eventually(&self, path: &str, holds: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let body = self.api(path).await.json();
            if holds(&body) {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "{path} never satisfied the condition: {body}"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }
}

fn client<'a>(body: &'a Value, ip: &str) -> Option<&'a Value> {
    body["clients"]
        .as_array()?
        .iter()
        .find(|row| row["ip"] == ip)
}

#[derive(Default)]
struct BodyReader {
    head: Vec<u8>,
    body: usize,
}

impl BodyReader {
    async fn read_until(&mut self, stream: &mut TcpStream, body: usize) {
        let mut chunk = vec![0_u8; CHUNK];
        while self.body < body {
            let read = stream.read(&mut chunk).await.expect("response bytes");
            assert!(read > 0, "the proxy closed the response early");
            let from = if self.head.ends_with(b"\r\n\r\n") {
                0
            } else {
                let before = self.head.len();
                self.head.extend_from_slice(&chunk[..read]);
                self.head
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map_or(read, |end| {
                        self.head.truncate(end + 4);
                        end + 4 - before
                    })
            };
            if from <= read {
                self.body += read - from;
            }
        }
    }
}

async fn get_small(stream: &mut TcpStream) {
    stream
        .write_all(b"GET /small HTTP/1.1\r\nHost: web.test\r\n\r\n")
        .await
        .expect("request");
    let mut seen = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !seen.ends_with(b"\r\n\r\nok") {
        let read = stream.read(&mut chunk).await.expect("response");
        assert!(read > 0, "the proxy closed the connection");
        seen.extend_from_slice(&chunk[..read]);
    }
}

#[tokio::test]
async fn a_keepalive_client_is_listed_and_its_counters_survive_its_connection() {
    let setup = Setup::start(None).await;
    let mut stream = setup.client().await;
    for _ in 0..3 {
        get_small(&mut stream).await;
    }

    let clients = setup
        .eventually("/api/v1/clients", |body| {
            client(body, CLIENT_IP).is_some_and(|row| row["requests"] == 3)
        })
        .await;
    let row = client(&clients, CLIENT_IP).expect("the client");
    assert_eq!(row["connections"], 1);
    assert_eq!(row["connectionsTotal"], 1);
    assert_eq!(row["bytesOut"], 6);
    assert_eq!(row["errors"], 0);
    assert_eq!(row["listeners"], json!(["traffic"]));
    assert_eq!(row["protocols"], json!(["http/1.1"]));
    assert_eq!(clients["ipv6Prefix"], 128);
    assert_eq!(clients["truncated"], false);
    assert_eq!(clients["totals"]["clients"], clients["trackedClients"]);
    assert!(clients["generation"].as_u64().is_some());

    let connections = setup
        .eventually(
            &format!("/api/v1/connections?ip={CLIENT_IP}&listener=traffic"),
            |body| body["total"] == 1,
        )
        .await;
    let connection = &connections["connections"][0];
    assert_eq!(connection["peerIp"], CLIENT_IP);
    assert_eq!(connection["listener"], "traffic");
    assert_eq!(connection["service"], "web");
    assert_eq!(connection["protocol"], "http/1.1");
    assert_eq!(connection["state"], "idle");
    assert_eq!(connection["requests"], 3);
    assert_eq!(connection["tls"], Value::Null);
    assert_eq!(connection["currentRequest"], Value::Null);

    drop(stream);
    let after = setup
        .eventually("/api/v1/clients", |body| {
            client(body, CLIENT_IP).is_some_and(|row| row["connections"] == 0)
        })
        .await;
    let row = client(&after, CLIENT_IP).expect("the client after it left");
    assert_eq!(
        (
            row["connectionsTotal"].clone(),
            row["requests"].clone(),
            row["bytesOut"].clone()
        ),
        (json!(1), json!(3), json!(6))
    );
    assert_eq!(setup.api("/api/v1/connections").await.json()["total"], 0);
}

#[tokio::test]
async fn bytes_and_the_current_request_are_visible_while_a_long_response_is_in_flight() {
    let setup = Setup::start(None).await;
    let mut stream = setup.client().await;
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: web.test\r\n\r\n")
        .await
        .expect("request");
    let mut body = BodyReader::default();
    body.read_until(&mut stream, CHUNK).await;

    let connections = setup
        .eventually(&format!("/api/v1/connections?ip={CLIENT_IP}"), |body| {
            body["connections"][0]["currentRequest"]["path"] == "/slow"
        })
        .await;
    let connection = &connections["connections"][0];
    assert_eq!(connection["state"], "active");
    assert_eq!(connection["currentRequest"]["method"], "GET");
    assert_eq!(connection["upstream"], "alpha");

    let live = setup
        .eventually("/api/v1/clients", |body| {
            client(body, CLIENT_IP)
                .is_some_and(|row| row["bytesOut"].as_u64() >= Some(CHUNK as u64))
        })
        .await;
    let row = client(&live, CLIENT_IP).expect("the client");
    assert!(row["bytesOut"].as_u64() < Some(2 * CHUNK as u64));
    assert_eq!(row["requests"], 0);

    setup.gate.notify_waiters();
    body.read_until(&mut stream, 2 * CHUNK).await;
    let done = setup
        .eventually("/api/v1/clients", |body| {
            client(body, CLIENT_IP).is_some_and(|row| row["requests"] == 1)
        })
        .await;
    assert_eq!(
        client(&done, CLIENT_IP).expect("the client")["bytesOut"],
        2 * CHUNK as u64
    );
}

#[tokio::test]
async fn the_endpoints_answer_an_explicit_error_when_tracking_is_disabled() {
    let setup = Setup::start(Some(ClientTracking {
        enabled: Some(false),
        ..ClientTracking::default()
    }))
    .await;
    let mut stream = setup.client().await;
    get_small(&mut stream).await;
    for path in ["/api/v1/clients", "/api/v1/connections"] {
        let response = setup.api(path).await;
        assert_eq!(response.status, 409, "{path}");
        assert_eq!(response.json()["error"]["code"], "client_tracking_disabled");
    }
}

#[tokio::test]
async fn invalid_queries_are_rejected_and_the_endpoints_need_the_token() {
    let setup = Setup::start(None).await;
    for path in [
        "/api/v1/clients?limit=0",
        "/api/v1/clients?limit=2001",
        "/api/v1/clients?sort=speed",
        "/api/v1/clients?bogus=1",
        "/api/v1/connections?limit=5001",
    ] {
        assert_eq!(setup.api(path).await.status, 400, "{path}");
    }
    for path in ["/api/v1/clients", "/api/v1/connections"] {
        let response = http_request(setup.management, "GET", path, &[], &[]).await;
        assert_eq!(response.status, 401, "{path}");
    }
}

#[tokio::test]
async fn peer_addresses_appear_only_on_the_two_authenticated_endpoints() {
    let setup = Setup::start(None).await;
    let mut stream = setup.client().await;
    get_small(&mut stream).await;
    setup
        .eventually("/api/v1/connections", |body| body["total"] == 1)
        .await;

    let own = setup.api("/api/v1/clients").await.text();
    assert!(own.contains(CLIENT_IP), "{own}");
    assert!(
        setup
            .api("/api/v1/connections")
            .await
            .text()
            .contains(CLIENT_IP)
    );

    let mut documents = Vec::new();
    for path in [
        "/metrics",
        "/status.json",
        "/ready",
        "/api/v1/stats",
        "/api/v1/status",
        "/api/v1/monitoring",
        "/api/v1/topology",
        "/api/v2/events",
        "/api/v1/events",
    ] {
        documents.push((path, setup.api(path).await));
    }
    for path in ["/haproxy/snapshot.json", "/haproxy/stats.csv", "/haproxy"] {
        documents.push((path, http_request(setup.page, "GET", path, &[], &[]).await));
    }
    for (path, response) in documents {
        assert!(
            !response.text().contains(CLIENT_IP),
            "{path} leaked the peer address"
        );
    }
    drop(stream);
    assert!(!setup.server.stderr().contains(CLIENT_IP));
}

#[tokio::test]
async fn an_upgraded_connection_is_listed_as_a_websocket() {
    let setup = Setup::start(None).await;
    let mut stream = setup.client().await;
    stream
        .write_all(
            b"GET /socket HTTP/1.1\r\nHost: web.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
        )
        .await
        .expect("upgrade request");
    let mut seen = Vec::new();
    let mut chunk = [0_u8; 256];
    while !seen.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await.expect("upgrade answer");
        assert!(read > 0, "the proxy closed the upgrade");
        seen.extend_from_slice(&chunk[..read]);
    }
    stream.write_all(b"hello").await.expect("tunnel write");
    let mut echo = [0_u8; 5];
    stream.read_exact(&mut echo).await.expect("tunnel echo");

    let connections = setup
        .eventually("/api/v1/connections", |body| {
            body["connections"][0]["protocol"] == "websocket"
        })
        .await;
    assert_eq!(connections["connections"][0]["state"], "active");
    drop(stream);
    let after = setup
        .eventually("/api/v1/clients", |body| {
            client(body, CLIENT_IP).is_some_and(|row| row["connections"] == 0)
        })
        .await;
    let row = client(&after, CLIENT_IP).expect("the client");
    assert_eq!(row["protocols"], json!(["websocket"]));
    assert_eq!(
        (row["bytesIn"].clone(), row["bytesOut"].clone()),
        (json!(5), json!(5))
    );
    assert_eq!(row["errors"], 0);
}
