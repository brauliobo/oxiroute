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

#[tokio::test]
async fn clients_and_connections_say_so_when_the_server_does_not_track_them() {
    let farm = Farm::start_with_traffic().await;
    for (view, header) in [("clients", "CLIENT"), ("conns", "CLIENT")] {
        let output = farm.top(TOKEN, &["top", "--view", view]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = stdout(&output);
        let lacks = text.contains("the server lacks per-");
        let tracks = text.contains(&format!("\n{header}"));
        assert!(lacks || tracks, "{view}: {text}");
        let json = document(&farm.top(TOKEN, &["top", "--view", view, "--json"]));
        assert_eq!(json["view"], view);
        assert!(json["notice"].is_string() || json["rows"].is_array());
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
