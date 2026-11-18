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
    server: ServerProcess,
    management: SocketAddr,
    token_file: PathBuf,
    draft: ConfigDraft,
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
        Self::start_on(reserve_tcp_address()).await
    }

    async fn start_on(management: SocketAddr) -> Self {
        let traffic = reserve_tcp_address();
        let backends = [backend().await, backend().await];
        let draft = config(management, traffic, backends);
        let validated = draft.clone().validate().expect("valid CLI test config");
        let mut server = ServerProcess::start(&validated, Some(TOKEN));
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(traffic).await;
        let token_file = server.token_path.clone().expect("token file");
        Self {
            server,
            management,
            token_file,
            draft,
        }
    }

    fn rewrite_config(&mut self, max_connections: u64) {
        self.draft.max_connections = Some(max_connections);
        let validated = self
            .draft
            .clone()
            .validate()
            .expect("valid CLI test config");
        process_support::write_config(&self.server.config_path, &validated);
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
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.starts_with("oxiroute: ") && message.contains("rejected.json is invalid"),
        "{message}"
    );
    assert!(
        message.contains("E_INVALID_VALUE") && message.contains("ListenerBind"),
        "{message}"
    );
}

#[tokio::test]
async fn tables_are_aligned_columns_without_json_and_plain_is_one_record_per_line() {
    let daemon = Daemon::start().await;
    let table = daemon.run(&["server", "list"]);
    assert!(table.status.success(), "{}", stderr(&table));
    let text = stdout(&table);
    let mut lines = text.lines();
    let header = lines.next().expect("header");
    assert!(header.starts_with("POOL  SERVER  ADDRESS"), "{text}");
    assert!(
        header.contains("HEALTH") && header.contains("WEIGHT"),
        "{text}"
    );
    assert_eq!(lines.count(), 2, "{text}");
    assert!(!text.contains('{') && !text.contains('['), "{text}");

    let plain = daemon.run(&["--output", "plain", "server", "list"]);
    let records = stdout(&plain);
    assert_eq!(records.lines().count(), 2, "{records}");
    assert!(
        records
            .lines()
            .all(|line| line.starts_with("pool=farm server=")),
        "{records}"
    );
    assert!(
        records.contains("weight=1") && records.contains("max=4"),
        "{records}"
    );
}

#[tokio::test]
async fn status_leads_with_the_build_the_active_revision_and_the_listeners() {
    let daemon = Daemon::start().await;
    let json = daemon.run(&["--output", "json", "status"]);
    let status = serde_json::from_slice::<Value>(&json.stdout).expect("status JSON");
    let revision = status["activeRevision"].as_str().expect("active revision");
    let text = stdout(&daemon.run(&["status"]));
    assert!(text.starts_with("build "), "{text}");
    assert!(text.contains("state           healthy"), "{text}");
    assert!(text.contains(&revision[..12]), "{text}");
    assert!(text.contains("\n\nNAME     PROTOCOL  BIND"), "{text}");
    assert!(text.contains("traffic  http"), "{text}");
}

#[tokio::test]
async fn an_empty_result_is_an_explicit_line_on_stderr_and_not_a_failure() {
    let daemon = Daemon::start().await;
    for (words, notice) in [
        (&["tls", "list"][..], "no certificates"),
        (&["requests", "list"], "no requests"),
        (&["rtmp", "stream", "list"], "no streams"),
    ] {
        let output = daemon.run(words);
        assert!(output.status.success(), "{words:?}: {}", stderr(&output));
        assert!(stdout(&output).is_empty(), "{words:?}: {}", stdout(&output));
        assert_eq!(stderr(&output).trim_end(), notice, "{words:?}");
    }
}

#[tokio::test]
async fn an_unknown_request_id_is_not_found() {
    let daemon = Daemon::start().await;
    let output = daemon.run(&["requests", "show", "no-such-request"]);
    assert_eq!(output.status.code(), Some(6), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("no-such-request"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn mutations_name_their_targets_and_report_what_really_changed() {
    let daemon = Daemon::start().await;
    let drain = || daemon.run(&["server", "drain", "--pool", "farm", "alpha"]);
    let first = stdout(&drain());
    assert!(first.starts_with("TARGET"), "{first}");
    assert!(first.contains("farm/alpha"), "{first}");
    assert!(first.trim_end().ends_with("changed"), "{first}");
    let again = stdout(&drain());
    assert!(again.trim_end().ends_with("unchanged"), "{again}");

    let pools = daemon.run(&["--output", "plain", "pool", "drain", "farm"]);
    assert_eq!(
        stdout(&pools),
        "target=farm setting=state value=drain change=changed\n"
    );
    let ready = daemon.run(&["--output", "plain", "pool", "ready", "farm"]);
    assert_eq!(
        stdout(&ready),
        "target=farm setting=state value=ready change=changed\n"
    );

    let weight = daemon.run(&[
        "--output", "plain", "server", "weight", "set", "--pool", "farm", "alpha", "7",
    ]);
    assert_eq!(
        stdout(&weight),
        "target=farm/alpha setting=weight value=7 change=changed\n"
    );
    let listener = daemon.run(&["--output", "plain", "listener", "drain", "traffic"]);
    assert_eq!(
        stdout(&listener),
        "target=traffic setting=state value=drain change=changed\n"
    );
}

#[tokio::test]
async fn json_mutations_pass_the_server_body_through() {
    let daemon = Daemon::start().await;
    let output = daemon.run(&["--output", "json", "pool", "drain", "farm"]);
    let body = serde_json::from_slice::<Value>(&output.stdout).expect("JSON");
    assert_eq!(body["outcome"], "applied", "{body}");
    assert_eq!(body["changed"], 1, "{body}");
}

#[tokio::test]
async fn metrics_print_the_prometheus_text_as_the_server_wrote_it() {
    let daemon = Daemon::start().await;
    let output = daemon.run(&["metrics"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("oxiroute_build_info{version=\""), "{text}");
    assert!(text.lines().count() > 10, "{text}");
    let plain = stdout(&daemon.run(&["--output", "plain", "metrics"]));
    assert!(plain.starts_with("oxiroute_build_info"), "{plain}");
}

#[tokio::test]
async fn monitoring_and_config_get_summarise_instead_of_dumping() {
    let daemon = Daemon::start().await;
    let monitoring = stdout(&daemon.run(&["monitoring"]));
    assert!(monitoring.starts_with("uptime"), "{monitoring}");
    assert!(monitoring.contains("memory"), "{monitoring}");
    assert!(monitoring.contains("\nNAME  ALGORITHM"), "{monitoring}");
    assert!(!monitoring.contains('{'), "{monitoring}");
    let config = stdout(&daemon.run(&["config", "get"]));
    assert!(config.contains("listeners               1"), "{config}");
    assert!(config.contains("upstream pools          1"), "{config}");
    assert!(
        !config.contains('{') && config.lines().count() < 30,
        "{config}"
    );
}

#[tokio::test]
async fn a_daemon_that_never_answers_is_a_timeout_named_after_the_limit() {
    let daemon = Daemon::start().await;
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("silent listener");
    let endpoint = format!("http://{}", silent.local_addr().expect("address"));
    let started = std::time::Instant::now();
    let output = cli_command()
        .args(["--endpoint", &endpoint, "--token-file"])
        .arg(&daemon.token_file)
        .args(["--timeout", "300ms", "pool", "list"])
        .output()
        .expect("oxiroute CLI");
    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.starts_with("oxiroute: "), "{message}");
    assert!(message.contains("did not answer within 300ms"), "{message}");
    assert!(message.contains("--timeout"), "{message}");
    assert!(!message.contains("stopped answering"), "{message}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_refused_connection_is_not_reported_as_a_timeout() {
    let daemon = Daemon::start().await;
    let closed = reserve_tcp_address();
    let output = cli_command()
        .args(["--endpoint", &format!("http://{closed}"), "--token-file"])
        .arg(&daemon.token_file)
        .args(["--timeout", "2s", "pool", "list"])
        .output()
        .expect("oxiroute CLI");
    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("cannot connect to"), "{message}");
    assert!(message.contains("is the daemon running?"), "{message}");
    assert!(!message.contains("did not answer"), "{message}");
}

#[tokio::test]
async fn a_timeout_must_be_a_positive_duration() {
    for value in ["0s", "10", "soon"] {
        let output = cli_command()
            .args(["--timeout", value, "status"])
            .output()
            .expect("oxiroute CLI");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{value}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("--timeout"),
            "{value}: {}",
            stderr(&output)
        );
    }
}

#[tokio::test]
async fn ready_wait_returns_once_the_daemon_starts_answering() {
    let management = reserve_tcp_address();
    let waiting = cli_command()
        .args(["--endpoint", &format!("http://{management}")])
        .args(["--timeout", "30s", "ready", "--wait"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("oxiroute CLI");
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let _daemon = Daemon::start_on(management).await;
    let output = tokio::task::spawn_blocking(move || waiting.wait_with_output())
        .await
        .expect("join")
        .expect("oxiroute CLI");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("ready"), "{}", stdout(&output));
}

#[tokio::test]
async fn ready_wait_gives_up_after_the_timeout_and_names_the_last_failure() {
    let closed = reserve_tcp_address();
    let output = cli_command()
        .args(["--endpoint", &format!("http://{closed}")])
        .args(["--timeout", "600ms", "ready", "--wait"])
        .output()
        .expect("oxiroute CLI");
    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.contains("readiness did not complete within 600ms"),
        "{message}"
    );
    assert!(message.contains("cannot connect to"), "{message}");
}

#[tokio::test]
async fn ready_wait_on_a_draining_daemon_times_out_with_the_http_status() {
    let daemon = Daemon::start().await;
    assert!(daemon.run(&["drain"]).status.success());
    let output = daemon.run(&["--timeout", "600ms", "ready", "--wait"]);
    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("the daemon answered HTTP 503"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn generation_reload_wait_prints_the_status_of_the_new_revision() {
    let mut daemon = Daemon::start().await;
    let revision = |daemon: &Daemon| {
        let output = daemon.run(&["--output", "json", "generation", "status"]);
        let status = serde_json::from_slice::<Value>(&output.stdout).expect("status JSON");
        status["generation"]["activeRevision"]
            .as_str()
            .expect("revision")
            .to_owned()
    };
    let before = revision(&daemon);
    daemon.rewrite_config(77);
    let output = daemon.run(&[
        "--output",
        "json",
        "generation",
        "reload",
        "--wait",
        "--timeout",
        "30s",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let status = serde_json::from_slice::<Value>(&output.stdout).expect("status JSON");
    let after = status["generation"]["activeRevision"]
        .as_str()
        .expect("revision");
    assert_ne!(after, before);
    assert_eq!(after, revision(&daemon));
    assert!(
        stderr(&output).contains("waiting for the candidate revision"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn generation_reload_wait_succeeds_at_once_when_the_file_is_already_active() {
    let daemon = Daemon::start().await;
    let output = daemon.run(&["generation", "reload", "--wait", "--timeout", "10s"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("accepting"), "{}", stdout(&output));
}
