//! `oxiroute context` and the precedence flags > variables > context > discovery, against a real daemon.

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
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use oxiroute_config::{ConfigDraft, Management};
use oxiroute_config_source::{ConfigFormat, render_config};
use tempfile::TempDir;

use config_support::empty_config;
use process_support::{ServerProcess, reserve_tcp_address};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

struct Daemon {
    server: ServerProcess,
    address: SocketAddr,
    token_file: PathBuf,
    home: TempDir,
}

impl Daemon {
    async fn start() -> Self {
        let address = reserve_tcp_address();
        let draft = ConfigDraft {
            management: Some(Management {
                bind: address,
                ui_dir: None,
                token_file: None,
            }),
            ..empty_config()
        };
        let text = render_config(
            ConfigFormat::Kdl,
            &draft.validate().expect("valid configuration"),
        )
        .expect("rendered configuration");
        let mut server = ServerProcess::start_authored("oxiroute.kdl", &text, TOKEN);
        server.wait_for_tcp(address).await;
        let token_file = server.token_path.clone().expect("token file");
        Self {
            server,
            address,
            token_file,
            home: TempDir::new().expect("home directory"),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn contexts_file(&self) -> PathBuf {
        self.home.path().join("oxiroute").join("contexts")
    }

    fn command(&self, words: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_oxiroute"));
        command
            .env_remove("OXIROUTE_ENDPOINT")
            .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
            .env_remove("OXIROUTE_CONTEXT")
            .env("XDG_CONFIG_HOME", self.home.path())
            .args(words);
        command
    }

    fn run(&self, words: &[&str]) -> Output {
        self.command(words).output().expect("oxiroute CLI")
    }

    fn token(&self) -> &str {
        self.token_file.to_str().expect("UTF-8 path")
    }

    fn add(&self, name: &str, address: &str, token: &[&str]) {
        let mut words = vec!["context", "add", name, "--address", address];
        words.extend_from_slice(token);
        let output = self.run(&words);
        assert_eq!(code(&output), 0, "{}", report(&output));
    }
}

fn dead_address() -> String {
    format!("http://{}", reserve_tcp_address())
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn report(output: &Output) -> String {
    format!("stdout: {}\nstderr: {}", out(output), err(output))
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

#[tokio::test(flavor = "multi_thread")]
async fn the_current_context_supplies_the_address_and_the_token_file() {
    let daemon = Daemon::start().await;
    daemon.add(
        "prod",
        &daemon.url(),
        &["--token-from-file", daemon.token(), "--use"],
    );

    let output = daemon.run(&["generation", "status"]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    assert!(out(&output).contains("active"), "{}", out(&output));
    assert_eq!(mode(&daemon.contexts_file()), 0o600);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_beat_variables_beat_the_context_beat_discovery() {
    let daemon = Daemon::start().await;
    let dead = dead_address();
    daemon.add(
        "prod",
        &daemon.url(),
        &["--token-from-file", daemon.token(), "--use"],
    );
    daemon.add("dead", &dead, &["--token-from-file", daemon.token()]);

    let by_context = daemon.run(&["--context", "dead", "generation", "status"]);
    assert_eq!(code(&by_context), 4, "{}", report(&by_context));

    let mut by_variable = daemon.command(&["generation", "status"]);
    by_variable
        .env("OXIROUTE_ENDPOINT", &dead)
        .env("OXIROUTE_MANAGEMENT_TOKEN_FILE", daemon.token());
    let by_variable = by_variable.output().expect("CLI");
    assert_eq!(code(&by_variable), 4, "{}", report(&by_variable));

    let mut flag_over_variable = daemon.command(&[
        "--endpoint",
        &daemon.url(),
        "--token-file",
        daemon.token(),
        "generation",
        "status",
    ]);
    flag_over_variable.env("OXIROUTE_ENDPOINT", &dead);
    let flag_over_variable = flag_over_variable.output().expect("CLI");
    assert_eq!(
        code(&flag_over_variable),
        0,
        "{}",
        report(&flag_over_variable)
    );

    let mut flag_over_context = daemon.command(&[
        "--context",
        "dead",
        "--endpoint",
        &daemon.url(),
        "--token-file",
        daemon.token(),
        "generation",
        "status",
    ]);
    flag_over_context.env("OXIROUTE_CONTEXT", "prod");
    let flag_over_context = flag_over_context.output().expect("CLI");
    assert_eq!(
        code(&flag_over_context),
        0,
        "{}",
        report(&flag_over_context)
    );

    let mut flag_over_environment = daemon.command(&["--context", "prod", "generation", "status"]);
    flag_over_environment.env("OXIROUTE_CONTEXT", "dead");
    let flag_over_environment = flag_over_environment.output().expect("CLI");
    assert_eq!(
        code(&flag_over_environment),
        0,
        "{}",
        report(&flag_over_environment)
    );

    let mut environment_over_current = daemon.command(&["generation", "status"]);
    environment_over_current.env("OXIROUTE_CONTEXT", "dead");
    let environment_over_current = environment_over_current.output().expect("CLI");
    assert_eq!(
        code(&environment_over_current),
        4,
        "{}",
        report(&environment_over_current)
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_context_token_never_goes_to_an_endpoint_given_by_flag() {
    let daemon = Daemon::start().await;
    daemon.add(
        "prod",
        &dead_address(),
        &["--token-from-file", daemon.token(), "--use"],
    );

    let output = daemon.run(&["--endpoint", &daemon.url(), "generation", "status"]);

    assert_eq!(code(&output), 3, "{}", report(&output));
    assert!(
        err(&output).contains("management token"),
        "{}",
        err(&output)
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_token_comes_from_a_variable_or_a_command_and_the_timeout_from_the_context() {
    let daemon = Daemon::start().await;
    daemon.add(
        "viaenv",
        &daemon.url(),
        &["--token-from-env", "OXIROUTE_TEST_CONTEXT_TOKEN"],
    );
    let mut command = daemon.command(&["--context", "viaenv", "generation", "status"]);
    command.env("OXIROUTE_TEST_CONTEXT_TOKEN", TOKEN);
    let by_env = command.output().expect("CLI");
    assert_eq!(code(&by_env), 0, "{}", report(&by_env));
    let unset = daemon.run(&["--context", "viaenv", "generation", "status"]);
    assert_eq!(code(&unset), 3, "{}", report(&unset));
    assert!(err(&unset).contains("not set"), "{}", err(&unset));

    daemon.add(
        "viacommand",
        &daemon.url(),
        &["--token-from-command", "cat", daemon.token()],
    );
    let by_command = daemon.run(&["--context", "viacommand", "generation", "status"]);
    assert_eq!(code(&by_command), 0, "{}", report(&by_command));

    let timed = daemon.run(&[
        "context",
        "add",
        "timed",
        "--address",
        &daemon.url(),
        "--token-from-file",
        daemon.token(),
        "--default-timeout",
        "3s",
        "--use",
    ]);
    assert_eq!(code(&timed), 0, "{}", report(&timed));
    let json = daemon.run(&["--output", "json", "context", "list"]);
    let listing: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON listing");
    let timed = listing["contexts"]
        .as_array()
        .expect("contexts")
        .iter()
        .find(|context| context["name"] == "timed")
        .expect("timed context");
    assert_eq!(timed["timeout"], "3s");
    let status = daemon.run(&["generation", "status"]);
    assert_eq!(code(&status), 0, "{}", report(&status));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn list_current_use_and_remove_manage_the_file_without_printing_a_token() {
    let daemon = Daemon::start().await;
    daemon.add(
        "prod",
        "http://192.0.2.10:9900",
        &["--token-from-file", "/etc/oxiroute/prod.token"],
    );
    daemon.add(
        "staging",
        "http://192.0.2.20:9900",
        &["--token-from-env", "STAGING_TOKEN"],
    );

    let none = daemon.run(&["context", "current"]);
    assert_eq!(code(&none), 3, "{}", report(&none));
    assert_eq!(code(&daemon.run(&["context", "use", "staging"])), 0);
    let current = daemon.run(&["context", "current"]);
    assert_eq!(out(&current), "staging\n", "{}", report(&current));

    let list = daemon.run(&["context", "list"]);
    let text = out(&list);
    assert!(
        text.contains("prod") && text.contains("192.0.2.10:9900"),
        "{text}"
    );
    assert!(text.contains("env:STAGING_TOKEN"), "{text}");
    assert!(text.contains("file:/etc/oxiroute/prod.token"), "{text}");
    let json = daemon.run(&["--output", "json", "context", "list"]);
    let listing: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON listing");
    assert_eq!(listing["current"], "staging");
    assert_eq!(listing["contexts"][1]["tokenSource"], "env");
    assert!(!out(&json).contains(TOKEN));

    let again = daemon.run(&[
        "context",
        "add",
        "prod",
        "--address",
        "http://192.0.2.11:9900",
    ]);
    assert_eq!(code(&again), 3, "{}", report(&again));
    assert!(err(&again).contains("--replace"), "{}", err(&again));
    let replaced = daemon.run(&[
        "context",
        "add",
        "prod",
        "--address",
        "http://192.0.2.11:9900",
        "--replace",
    ]);
    assert_eq!(code(&replaced), 0, "{}", report(&replaced));

    assert_eq!(code(&daemon.run(&["context", "remove", "staging"])), 0);
    let missing = daemon.run(&["context", "use", "staging"]);
    assert_eq!(code(&missing), 3, "{}", report(&missing));
    let current = daemon.run(&["context", "current"]);
    assert_eq!(code(&current), 3, "{}", report(&current));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_readable_contexts_file_or_an_inline_token_stops_every_command() {
    let daemon = Daemon::start().await;
    daemon.add(
        "prod",
        &daemon.url(),
        &["--token-from-file", daemon.token()],
    );
    let file = daemon.contexts_file();

    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
    let loose = daemon.run(&["--context", "prod", "generation", "status"]);
    assert_eq!(code(&loose), 3, "{}", report(&loose));
    assert!(err(&loose).contains("chmod 600"), "{}", err(&loose));

    fs::write(
        &file,
        format!(
            r#"{{"schemaVersion":1,"contexts":{{"prod":{{"endpoint":"{}","token":"{TOKEN}"}}}}}}"#,
            daemon.url()
        ),
    )
    .expect("inline token");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    let inline = daemon.run(&["--context", "prod", "generation", "status"]);
    assert_eq!(code(&inline), 3, "{}", report(&inline));
    assert!(!report(&inline).contains(TOKEN), "{}", report(&inline));
    daemon.server.shutdown();
}
