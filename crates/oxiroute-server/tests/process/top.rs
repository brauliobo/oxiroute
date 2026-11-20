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
    ClientTracking, ConfigDraft, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction,
    HttpService, Listener, Management, Protocol, UpstreamPool,
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
    traffic: SocketAddr,
    held: Vec<TcpStream>,
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
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REPLY}",
                    REPLY.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    address
}

fn config(
    management: SocketAddr,
    traffic: SocketAddr,
    backends: [SocketAddr; 2],
    client_tracking: Option<ClientTracking>,
) -> ConfigDraft {
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
        client_tracking,
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

const UPLOAD: &str = "hello";
const REPLY: &str = "ok";

fn post(path: &str, headers: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: example.test\r\n{headers}Content-Length: {}\r\n\r\n{UPLOAD}",
        UPLOAD.len()
    )
    .into_bytes()
}

fn post_through(traffic: SocketAddr) -> String {
    let mut stream = TcpStream::connect(traffic).expect("traffic connection");
    stream
        .write_all(&post("/hello", "Connection: close\r\n"))
        .expect("request");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("answer");
    answer
}

impl Farm {
    async fn start_with_traffic() -> Self {
        Self::start(None).await
    }

    async fn start(client_tracking: Option<ClientTracking>) -> Self {
        let management = reserve_tcp_address();
        let traffic = reserve_tcp_address();
        let backends = [backend().await, backend().await];
        let validated = config(management, traffic, backends, client_tracking)
            .validate()
            .expect("valid top config");
        let mut server = ServerProcess::start(&validated, Some(TOKEN));
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(traffic).await;
        let farm = Self {
            _server: server,
            state: TempDir::new().expect("scratch"),
            management,
            traffic,
            held: Vec::new(),
        };
        tokio::task::spawn_blocking(move || {
            for _ in 0..REQUESTS {
                assert!(
                    post_through(traffic).starts_with("HTTP/1.1 200"),
                    "traffic is proxied"
                );
            }
        })
        .await
        .expect("traffic task");
        farm
    }

    async fn hold_a_connection(&mut self) {
        let traffic = self.traffic;
        let stream = tokio::task::spawn_blocking(move || {
            let mut stream = TcpStream::connect(traffic).expect("held connection");
            stream.write_all(&post("/held", "")).expect("held request");
            let mut seen = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !seen.ends_with(b"\r\n\r\nok") {
                let read = stream.read(&mut chunk).expect("held answer");
                assert!(read > 0, "the proxy closed the held connection");
                seen.extend_from_slice(&chunk[..read]);
            }
            stream
        })
        .await
        .expect("held task");
        self.held.push(stream);
    }

    fn layout_file(&self) -> std::path::PathBuf {
        self.state
            .path()
            .join("xdg")
            .join("oxiroute")
            .join("top.json")
    }

    fn top(&self, token: &str, words: &[&str]) -> Output {
        let token_file = write_token(self.state.path(), token, 0o600);
        Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .env_remove("OXIROUTE_ENDPOINT")
            .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
            .env("XDG_STATE_HOME", self.state.path().join("xdg"))
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

fn section<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    text.split(&format!("\n{name}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no {name} section in:\n{text}"))
        .lines()
        .take_while(|line| !line.is_empty())
        .collect()
}

fn column(text: &str, name: &str, title: &str) -> Vec<(String, String)> {
    let lines = section(text, name);
    let header: Vec<_> = lines[0].split_whitespace().collect();
    let at = header
        .iter()
        .position(|candidate| *candidate == title)
        .expect("column");
    lines[1..]
        .iter()
        .filter(|line| !line.starts_with("TOTAL") && !line.starts_with("other"))
        .map(|line| {
            let cells: Vec<_> = line.split_whitespace().collect();
            (cells[0].to_owned(), cells[at].to_owned())
        })
        .collect()
}

fn document(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON document")
}

fn rows(document: &Value) -> Vec<&str> {
    document["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["key"].as_str().expect("key"))
        .collect()
}

#[tokio::test]
async fn once_prints_the_listeners_and_the_upstream_tree_measured_over_a_real_window() {
    let farm = Farm::start_with_traffic().await;
    let output = farm.top(TOKEN, &["top", "--once"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = stdout(&output);
    assert!(text.starts_with("oxiroute "), "{text}");
    assert!(text.contains("\nrates over a 2s window\n"), "{text}");
    assert_eq!(
        column(&text, "Listeners", "STATE"),
        [("traffic".to_owned(), "up".to_owned())]
    );
    let upstreams = column(&text, "Upstreams", "2XX");
    let names: Vec<_> = upstreams.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["farm", "alpha", "beta"]);
    assert_eq!(upstreams[0].1, REQUESTS.to_string());
    let served: usize = upstreams[1..]
        .iter()
        .map(|(_, count)| count.parse::<usize>().expect("count"))
        .sum();
    assert_eq!(served, REQUESTS);
    let health = column(&text, "Upstreams", "HEALTH");
    assert!(
        health[1..].iter().all(|(_, health)| health == "unchecked"),
        "{health:?}"
    );
    let total = section(&text, "Upstreams")
        .into_iter()
        .find(|line| line.starts_with("TOTAL"))
        .expect("a TOTAL row");
    assert!(total.contains(&REQUESTS.to_string()), "{total}");
}

#[tokio::test]
async fn without_a_terminal_top_prints_one_snapshot_and_filters_and_sorts() {
    let farm = Farm::start_with_traffic().await;
    let piped = farm.top(TOKEN, &["top"]);
    assert!(piped.status.success());
    assert!(stdout(&piped).contains("\nUpstreams\n"));
    let filtered = stdout(&farm.top(
        TOKEN,
        &[
            "top",
            "--view",
            "upstreams",
            "--filter",
            "ALPHA",
            "--sort",
            "NAME",
        ],
    ));
    assert!(
        filtered.contains("alpha") && !filtered.contains("beta") && !filtered.contains("Listeners"),
        "{filtered}"
    );
    assert!(
        filtered.contains("farm"),
        "the parent of a match stays: {filtered}"
    );
    let requests = stdout(&farm.top(TOKEN, &["top", "--view", "requests"]));
    assert!(
        requests.contains("\nRequests\nCLIENT") && requests.contains("AGE"),
        "{requests}"
    );
    let bad_column = farm.top(TOKEN, &["top", "--sort", "nonsense"]);
    assert_eq!(bad_column.status.code(), Some(3));
}

#[tokio::test]
async fn json_is_one_document_with_the_measured_rows_of_the_view() {
    let farm = Farm::start_with_traffic().await;
    for words in [&["top", "--json"][..], &["--output", "json", "top"]] {
        let document = document(&farm.top(TOKEN, words));
        assert_eq!(document["schemaVersion"], 2);
        assert_eq!(document["view"], "listeners");
        assert_eq!(
            document["window"],
            json!({ "name": "2s", "ms": 2_000, "complete": true })
        );
        assert_eq!(rows(&document), ["traffic"]);
        assert_eq!(document["rows"][0]["values"]["state"], "up");
        assert_eq!(document["total"]["key"], "TOTAL");
        assert!(document["sampledAtUnixMs"].as_u64().expect("time") > 0);
        assert_eq!(document["instance"]["accepting"], true);
    }
    let upstreams = document(&farm.top(TOKEN, &["top", "--view", "upstreams", "--json"]));
    assert_eq!(rows(&upstreams), ["farm", "farm/alpha", "farm/beta"]);
    assert_eq!(upstreams["rows"][1]["parent"], "farm");
    assert_eq!(upstreams["rows"][1]["depth"], 1);
    assert_eq!(upstreams["rows"][0]["values"]["2xx"], REQUESTS);
    assert_eq!(upstreams["total"]["values"]["2xx"], REQUESTS);
    let columns = upstreams["columns"].as_array().expect("columns");
    let req = columns
        .iter()
        .find(|column| column["key"] == "req")
        .expect("req");
    assert_eq!(req["unit"], "perSecond");
}

#[tokio::test]
async fn csv_has_a_header_one_line_per_row_and_the_total() {
    let farm = Farm::start_with_traffic().await;
    let output = farm.top(TOKEN, &["top", "--view", "upstreams", "--csv"]);
    assert!(output.status.success());
    let text = stdout(&output);
    let lines: Vec<_> = text.lines().collect();
    assert!(lines[0].starts_with("kind,key,name,state,"), "{text}");
    let keys: Vec<_> = lines[1..]
        .iter()
        .map(|line| line.split(',').take(2).collect::<Vec<_>>().join(","))
        .collect();
    assert_eq!(
        keys,
        ["row,farm", "row,farm/alpha", "row,farm/beta", "total,TOTAL"]
    );
    let width = lines[0].split(',').count();
    assert!(lines.iter().all(|line| line.split(',').count() == width));
}

#[tokio::test]
async fn the_window_is_chosen_and_life_needs_no_wait() {
    let farm = Farm::start_with_traffic().await;
    let life = document(&farm.top(TOKEN, &["top", "--json", "--window", "life"]));
    assert_eq!(
        life["window"],
        json!({ "name": "life", "ms": null, "complete": true })
    );
    let rate = life["summary"]["rates"]["reqPerSec"]
        .as_f64()
        .expect("a rate");
    assert!(rate > 0.0, "{rate}");
}

fn assert_client_row(row: &Value) {
    let values = &row["values"];
    assert_eq!(values["ip"], "127.0.0.1");
    assert_eq!(values["reqs"], REQUESTS);
    assert_eq!(values["intot"], REQUESTS * UPLOAD.len());
    assert_eq!(values["outtot"], REQUESTS * REPLY.len());
    assert_eq!(values["listener"], "traffic");
    assert_eq!(values["proto"], "http/1.1");
    assert!(
        values["conns"].as_u64() >= Some(REQUESTS as u64),
        "{values}"
    );
}

#[tokio::test]
async fn the_clients_view_lists_the_real_client_with_its_bytes_in_json() {
    let farm = Farm::start_with_traffic().await;
    let document = document(&farm.top(TOKEN, &["top", "--view", "clients", "--json"]));
    assert_eq!(document["view"], "clients");
    assert!(document["notice"].is_null(), "{document}");
    assert_eq!(rows(&document), ["127.0.0.1"]);
    assert_client_row(&document["rows"][0]);
    assert_eq!(document["population"], json!({ "shown": 1, "of": 1 }));
    assert_eq!(document["total"]["key"], "TOTAL");
    assert_eq!(document["total"]["values"]["reqs"], REQUESTS);
    assert_eq!(
        document["total"]["values"]["intot"],
        document["rows"][0]["values"]["intot"]
    );
    assert_eq!(document["truncated"], false);
}

#[tokio::test]
async fn the_clients_view_prints_one_line_per_client_in_csv_and_text() {
    let farm = Farm::start_with_traffic().await;
    let csv = stdout(&farm.top(TOKEN, &["top", "--view", "clients", "--csv"]));
    let lines: Vec<_> = csv.lines().collect();
    let header: Vec<_> = lines[0].split(',').collect();
    let at = |title: &str| header.iter().position(|cell| *cell == title).expect(title);
    let client: Vec<_> = lines[1].split(',').collect();
    assert_eq!(&client[..2], ["row", "127.0.0.1"], "{csv}");
    assert_eq!(client[at("listener")], "traffic");
    assert_eq!(client[at("proto")], "http/1.1");
    assert_eq!(client[at("reqs")], REQUESTS.to_string());
    assert_eq!(client[at("intot")], (REQUESTS * UPLOAD.len()).to_string());
    assert_eq!(client[at("outtot")], (REQUESTS * REPLY.len()).to_string());
    assert!(lines[2].starts_with("total,TOTAL,"), "{csv}");
    assert_eq!(lines.len(), 3, "{csv}");

    let text = stdout(&farm.top(TOKEN, &["top", "--once", "--view", "clients"]));
    let rows = section(&text, "Clients");
    assert!(rows[0].starts_with("CLIENT"), "{text}");
    let cells: Vec<_> = rows[1].split_whitespace().collect();
    assert_eq!(cells[0], "127.0.0.1", "{text}");
    assert!(rows[2].starts_with("TOTAL"), "{text}");
    assert_eq!(rows.len(), 3, "{text}");
}

#[tokio::test]
async fn the_connections_view_lists_the_open_connection_with_its_bytes() {
    let mut farm = Farm::start_with_traffic().await;
    farm.hold_a_connection().await;
    let document = document(&farm.top(TOKEN, &["top", "--view", "conns", "--json"]));
    assert_eq!(document["view"], "conns");
    assert!(document["notice"].is_null(), "{document}");
    let keys = rows(&document);
    assert_eq!(keys.len(), 1, "{document}");
    let values = &document["rows"][0]["values"];
    let peer = values["ip"].as_str().expect("peer");
    assert!(peer.starts_with("127.0.0.1:"), "{peer}");
    assert_eq!(values["listener"], "traffic");
    assert_eq!(values["proto"], "http/1.1");
    assert_eq!(values["reqs"], 1);
    assert_eq!(values["intot"], UPLOAD.len());
    assert_eq!(values["outtot"], REPLY.len());
    assert_eq!(document["population"], json!({ "shown": 1, "of": 1 }));

    let csv = stdout(&farm.top(TOKEN, &["top", "--view", "conns", "--csv"]));
    let lines: Vec<_> = csv.lines().collect();
    assert!(lines[1].starts_with("row,"), "{csv}");
    assert!(
        lines[1].contains("127.0.0.1:") && lines[1].contains("traffic"),
        "{csv}"
    );

    let text = stdout(&farm.top(TOKEN, &["top", "--once", "--view", "conns"]));
    let rows = section(&text, "Conns");
    assert!(rows[0].starts_with("CLIENT"), "{text}");
    assert!(rows[1].starts_with("127.0.0.1:"), "{text}");
    assert!(
        rows[1].contains("traffic") && rows[1].contains("http/1.1"),
        "{text}"
    );
}

#[tokio::test]
async fn disabled_client_tracking_is_an_explicit_notice_in_every_output() {
    let farm = Farm::start(Some(ClientTracking {
        enabled: Some(false),
        ..ClientTracking::default()
    }))
    .await;
    for (view, what) in [("clients", "client"), ("conns", "connection")] {
        let notice = format!(
            "per-{what} tracking is disabled on the server: enable it in the configuration"
        );
        let json = document(&farm.top(TOKEN, &["top", "--view", view, "--json"]));
        assert_eq!(json["view"], view);
        assert_eq!(json["notice"], notice);
        assert_eq!(json["rows"], json!([]));
        let text = stdout(&farm.top(TOKEN, &["top", "--once", "--view", view]));
        assert!(text.contains(&notice), "{view}: {text}");
        let csv = farm.top(TOKEN, &["top", "--view", view, "--csv"]);
        assert!(csv.status.success());
        assert!(!stdout(&csv).contains("127.0.0.1"));
    }
}

#[tokio::test]
async fn the_saved_layout_shapes_the_printed_output_and_reset_forgets_it() {
    let farm = Farm::start_with_traffic().await;
    let file = farm.layout_file();
    std::fs::create_dir_all(file.parent().expect("directory")).expect("state directory");
    std::fs::write(
        &file,
        r#"{"version": 1, "panels": {"meters": "hidden"}, "columns": {"listeners": {"off": ["state"]}}}"#,
    )
    .expect("layout");
    let hidden = stdout(&farm.top(TOKEN, &["top", "--once", "--view", "listeners"]));
    assert!(hidden.starts_with("Listeners\n"), "{hidden}");
    assert!(!hidden.lines().nth(1).expect("header").contains("STATE"));
    let reset = stdout(&farm.top(TOKEN, &["top", "--once", "--reset-layout"]));
    assert!(reset.starts_with("oxiroute "), "{reset}");
    assert!(!file.exists());
}

#[tokio::test]
async fn a_damaged_layout_file_is_reported_and_the_defaults_are_used() {
    let farm = Farm::start_with_traffic().await;
    let file = farm.layout_file();
    std::fs::create_dir_all(file.parent().expect("directory")).expect("state directory");
    std::fs::write(&file, "not json").expect("layout");
    let output = farm.top(TOKEN, &["top", "--once"]);
    assert!(output.status.success());
    assert!(stdout(&output).starts_with("oxiroute "));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--reset-layout"), "{stderr}");
}

#[tokio::test]
async fn a_wrong_token_is_an_authentication_failure() {
    let farm = Farm::start_with_traffic().await;
    assert_eq!(
        farm.top(OTHER_TOKEN, &["top", "--once"]).status.code(),
        Some(5)
    );
}
