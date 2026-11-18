//! The `oxiroute` client commands against a real daemon: a listener, a pool of two servers and a management API.

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
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    path::PathBuf,
    process::{Command, Output},
};

use oxiroute_config::{
    ConfigDraft, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpService,
    Listener, Management, Protocol, UpstreamPool,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

use config_support::{empty_config, socket_bind};
use process_support::{ServerProcess, reserve_tcp_address};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

struct Daemon {
    _server: ServerProcess,
    management: SocketAddr,
    token_file: PathBuf,
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

impl Daemon {
    async fn start() -> Self {
        let management = reserve_tcp_address();
        let traffic = reserve_tcp_address();
        let backends = [backend().await, backend().await];
        let validated = config(management, traffic, backends)
            .validate()
            .expect("valid CLI test config");
        let mut server = ServerProcess::start(&validated, Some(TOKEN));
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(traffic).await;
        let token_file = server.token_path.clone().expect("token file");
        Self {
            _server: server,
            management,
            token_file,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.management)
    }

    fn connection(&self) -> [String; 4] {
        [
            "--endpoint".into(),
            self.endpoint(),
            "--token-file".into(),
            self.token_file.display().to_string(),
        ]
    }

    fn run(&self, words: &[&str]) -> Output {
        cli_command()
            .args(self.connection())
            .args(words)
            .output()
            .expect("oxiroute CLI")
    }

    fn run_with_flags_last(&self, words: &[&str]) -> Output {
        cli_command()
            .args(words)
            .args(self.connection())
            .output()
            .expect("oxiroute CLI")
    }
}

fn cli_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_oxiroute"));
    command
        .env_remove("OXIROUTE_ENDPOINT")
        .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE");
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn global_flags_work_before_and_after_the_subcommand() {
    let daemon = Daemon::start().await;
    for words in [
        &["--output", "json", "pool", "list"][..],
        &["--output", "json", "config", "get"],
        &["--output", "json", "listener", "show", "traffic"],
        &["--output", "json", "server", "list"],
    ] {
        let before = daemon.run(words);
        let (flag, rest) = words.split_at(2);
        let after = daemon.run_with_flags_last(&[rest, flag].concat());
        assert!(before.status.success(), "{words:?}: {}", stderr(&before));
        assert!(after.status.success(), "{words:?}: {}", stderr(&after));
        assert_eq!(
            serde_json::from_slice::<Value>(&before.stdout).expect("JSON"),
            serde_json::from_slice::<Value>(&after.stdout).expect("JSON"),
            "{words:?}"
        );
    }
}

#[tokio::test]
async fn check_and_top_take_the_connection_flags_after_their_name() {
    let daemon = Daemon::start().await;
    let check = daemon.run_with_flags_last(&["check", "--output", "json"]);
    assert!(
        matches!(check.status.code(), Some(0 | 1)),
        "{}",
        stdout(&check)
    );
    assert!(serde_json::from_slice::<Value>(&check.stdout).is_ok());
    let top = daemon.run_with_flags_last(&["top", "--once"]);
    assert!(top.status.success(), "{}", stderr(&top));
    assert!(stdout(&top).starts_with("oxiroute "), "{}", stdout(&top));
}

#[tokio::test]
async fn the_endpoint_environment_variable_replaces_the_flag() {
    let daemon = Daemon::start().await;
    let output = cli_command()
        .env("OXIROUTE_ENDPOINT", daemon.endpoint())
        .env("OXIROUTE_MANAGEMENT_TOKEN_FILE", &daemon.token_file)
        .args(["--output", "json", "pool", "list"])
        .output()
        .expect("oxiroute CLI");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).expect("JSON")["pools"][0]["name"],
        "farm"
    );
}

#[tokio::test]
async fn a_token_the_daemon_does_not_know_is_an_authentication_failure() {
    let daemon = Daemon::start().await;
    let other = daemon.token_file.with_file_name("other.token");
    std::fs::write(&other, format!("{}\n", "f".repeat(64))).expect("token");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let output = cli_command()
        .args([
            "--endpoint",
            &daemon.endpoint(),
            "pool",
            "list",
            "--token-file",
        ])
        .arg(&other)
        .output()
        .expect("oxiroute CLI");
    assert_eq!(output.status.code(), Some(5), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.starts_with("oxiroute: "), "{message}");
    assert!(message.contains("HTTP 401"), "{message}");
    assert!(message.contains("--token-file"), "{message}");
    assert!(!message.contains(&"f".repeat(64)));
}

#[tokio::test]
async fn ready_on_a_draining_daemon_prints_the_body_and_exits_with_the_remote_category() {
    let daemon = Daemon::start().await;
    let ready = daemon.run(&["--output", "json", "ready"]);
    assert!(ready.status.success(), "{}", stderr(&ready));

    assert!(daemon.run(&["drain"]).status.success());
    let draining = daemon.run(&["--output", "json", "ready"]);
    assert_eq!(draining.status.code(), Some(8), "{}", stderr(&draining));
    let body = serde_json::from_slice::<Value>(&draining.stdout).expect("the readiness body");
    assert_eq!(body["ready"], false, "{body}");
    assert!(
        stderr(&draining).contains("HTTP 503"),
        "{}",
        stderr(&draining)
    );
}

#[tokio::test]
async fn quiet_still_reports_a_missing_resource_on_stderr() {
    let daemon = Daemon::start().await;
    let output = daemon.run(&["--quiet", "pool", "show", "nosuch"]);
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    assert!(
        stderr(&output).starts_with("oxiroute: management endpoint returned HTTP 404"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn a_reader_that_stops_early_ends_the_command_without_a_panic() {
    let daemon = Daemon::start().await;
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let output = cli_command()
        .args(daemon.connection())
        .args(["pool", "list"])
        .stdout(writer)
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("oxiroute CLI");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[tokio::test]
async fn a_rejected_configuration_names_the_failing_setting() {
    let daemon = Daemon::start().await;
    let directory = tempfile::TempDir::new().expect("directory");
    let rejected = directory.path().join("rejected.json");
    std::fs::write(
        &rejected,
        r#"{"version":1,"upstream_pools":[{"name":"farm","servers":[]}],"listeners":[{"name":"x","bind":"nonsense"}]}"#,
    )
    .expect("config");
    let output = daemon.run(&["config", "validate", rejected.to_str().unwrap()]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.starts_with("oxiroute: management endpoint returned HTTP 4"),
        "{message}"
    );
    assert!(
        message.contains("E_INVALID_FIELD") && message.contains("ListenerBind"),
        "{message}"
    );
}
