//! `oxiroute top` against a real scratch process: real listeners, a real pool and real traffic.

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
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpStream},
    process::{Command, Output},
};

use oxiroute_config::{
    ConfigDraft, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpService,
    Listener, Management, Protocol, UpstreamPool,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

use config_support::{empty_config, socket_bind};
use process_support::{ServerProcess, reserve_tcp_address, write_token};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const OTHER_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REQUESTS: usize = 20;

struct Farm {
    _server: ServerProcess,
    state: TempDir,
    management: SocketAddr,
}

async fn backend() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("backend bind");
    let address = listener.local_addr().expect("backend address");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    address
}

fn config(management: SocketAddr, traffic: SocketAddr, backends: [SocketAddr; 2]) -> ConfigDraft {
    let servers = backends.iter().zip(["alpha", "beta"]).map(|(address, name)| {
        json!({ "name": name, "endpoint": { "type": "socket", "address": address.to_string() }, "max_connections": 4 })
    });
    let pool: UpstreamPool =
        serde_json::from_value(json!({ "name": "farm", "servers": servers.collect::<Vec<_>>() }))
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
        listeners: vec![Listener {
            name: "traffic".into(),
            bind: socket_bind(traffic),
            protocol: Protocol::Http,
            service: Some("traffic".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(100),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        upstream_pools: vec![pool],
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
}

fn get_through(traffic: SocketAddr) -> String {
    let mut stream = TcpStream::connect(traffic).expect("traffic connection");
    stream
        .write_all(b"GET /hello HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n")
        .expect("request");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("answer");
    answer
}

impl Farm {
    async fn start_with_traffic() -> Self {
        let management = reserve_tcp_address();
        let traffic = reserve_tcp_address();
        let backends = [backend().await, backend().await];
        let validated = config(management, traffic, backends)
            .validate()
            .expect("valid top config");
        let mut server = ServerProcess::start(&validated, Some(TOKEN));
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(traffic).await;
        let farm = Self {
            _server: server,
            state: TempDir::new().expect("scratch"),
            management,
        };
        tokio::task::spawn_blocking(move || {
            for _ in 0..REQUESTS {
                assert!(
                    get_through(traffic).starts_with("HTTP/1.1 200"),
                    "traffic is proxied"
                );
            }
        })
        .await
        .expect("traffic task");
        farm
    }

    fn top(&self, token: &str, words: &[&str]) -> Output {
        let token_file = write_token(self.state.path(), token, 0o600);
        Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .env_remove("OXIROUTE_ENDPOINT")
            .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
            .args(["--endpoint", &format!("http://{}", self.management)])
            .arg("--token-file")
            .arg(token_file)
            .args(words)
            .output()
            .expect("oxiroute CLI")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn column(table: &str, section: &str, name: &str) -> Vec<(String, String)> {
    let body = table
        .split(&format!("\n{section}\n"))
        .nth(1)
        .expect("section");
    let mut lines = body.lines().take_while(|line| !line.is_empty());
    let header: Vec<_> = lines.next().expect("header").split_whitespace().collect();
    let at = header
        .iter()
        .position(|title| *title == name)
        .expect("column");
    lines
        .map(|line| {
            let cells: Vec<_> = line.split_whitespace().collect();
            (cells[0].to_owned(), cells[at].to_owned())
        })
        .collect()
}

#[tokio::test]
async fn once_prints_listeners_pools_and_servers_with_the_counters_of_real_traffic() {
    let farm = Farm::start_with_traffic().await;
    let output = farm.top(TOKEN, &["top", "--once"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = stdout(&output);
    assert!(text.starts_with("oxiroute "), "{text}");
    assert_eq!(
        column(&text, "Listeners", "STATE"),
        [("traffic".to_owned(), "up".to_owned())]
    );
    let pool = column(&text, "Pools", "2XX");
    assert_eq!(pool, [("farm".to_owned(), REQUESTS.to_string())]);
    let servers = column(&text, "Servers", "2XX");
    let names: Vec<_> = servers.iter().map(|(pool, _)| pool.as_str()).collect();
    assert_eq!(names, ["farm", "farm"]);
    let served: usize = servers
        .iter()
        .map(|(_, count)| count.parse::<usize>().expect("count"))
        .sum();
    assert_eq!(served, REQUESTS);
    let statuses = column(&text, "Servers", "STATUS");
    assert!(
        statuses.iter().all(|(_, status)| status == "unchecked"),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn without_a_terminal_top_prints_one_snapshot_and_filters_and_sorts() {
    let farm = Farm::start_with_traffic().await;
    let piped = farm.top(TOKEN, &["top"]);
    assert!(piped.status.success());
    assert!(stdout(&piped).contains("\nServers\n"));
    let filtered = stdout(&farm.top(
        TOKEN,
        &[
            "top", "--view", "servers", "--filter", "ALPHA", "--sort", "SERVER",
        ],
    ));
    assert!(
        filtered.contains("alpha") && !filtered.contains("beta") && !filtered.contains("Pools"),
        "{filtered}"
    );
    let requests = stdout(&farm.top(TOKEN, &["top", "--view", "requests"]));
    assert!(
        requests.contains("Requests\nID") && requests.contains("AGE"),
        "{requests}"
    );
    let bad_column = farm.top(TOKEN, &["top", "--sort", "nonsense"]);
    assert_eq!(bad_column.status.code(), Some(3));
}

#[tokio::test]
async fn json_is_the_management_snapshot_document() {
    let farm = Farm::start_with_traffic().await;
    for words in [&["top", "--json"][..], &["--output", "json", "top"]] {
        let output = farm.top(TOKEN, words);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snapshot: Value = serde_json::from_slice(&output.stdout).expect("one JSON document");
        assert_eq!(snapshot["schemaVersion"], 1);
        assert_eq!(
            snapshot["pools"][0]["traffic"]["responses"]["2xx"],
            REQUESTS.to_string()
        );
        assert_eq!(
            snapshot["listeners"][0]["http"]["total"],
            REQUESTS.to_string()
        );
    }
}

#[tokio::test]
async fn a_wrong_token_is_an_authentication_failure() {
    let farm = Farm::start_with_traffic().await;
    assert_eq!(
        farm.top(OTHER_TOKEN, &["top", "--once"]).status.code(),
        Some(5)
    );
}
