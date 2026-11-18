//! Command-line parsing seen from the process: mistyped words, the path shorthand and unreadable files.

#[path = "../cli_support/mod.rs"]
mod cli_support;

use std::{
    fs,
    io::pipe,
    net::TcpListener,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use cli_support::cli;

const SERVES_FOREVER_UNLESS_REFUSED: Duration = Duration::from_secs(10);

fn run_bounded(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("oxiroute");
    let deadline = Instant::now() + SERVES_FOREVER_UNLESS_REFUSED;
    while child.try_wait().expect("wait").is_none() {
        if Instant::now() > deadline {
            child.kill().expect("kill");
            let output = child.wait_with_output().expect("output");
            panic!(
                "oxiroute kept running: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().expect("output")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn directory_with_a_servable_config() -> TempDir {
    let directory = TempDir::new().expect("directory");
    fs::write(directory.path().join("oxiroute.kdl"), "version 1\n").expect("config");
    directory
}

#[test]
fn a_mistyped_subcommand_is_a_usage_error_and_never_starts_the_daemon() {
    let directory = directory_with_a_servable_config();
    for (typo, suggestion) in [("stauts", "status"), ("servr", "server")] {
        let output = run_bounded(cli().current_dir(directory.path()).arg(typo));
        assert_eq!(output.status.code(), Some(2), "{typo}");
        let message = stderr(&output);
        assert!(message.contains("unrecognized subcommand"), "{message}");
        assert!(message.contains(suggestion), "{message}");
    }
}

#[test]
fn no_arguments_print_the_help_instead_of_serving_the_default_file() {
    let directory = directory_with_a_servable_config();
    let output = run_bounded(cli().current_dir(directory.path()));
    assert_eq!(output.status.code(), Some(2));
    let message = stderr(&output);
    assert!(message.contains("Usage: oxiroute"), "{message}");
    assert!(message.contains("Commands:"), "{message}");
}

#[test]
fn a_missing_configuration_names_its_path_and_the_os_cause() {
    let directory = TempDir::new().expect("directory");
    for arguments in [
        &["missing.lua"][..],
        &["serve", "missing.kdl"],
        &["config", "check", "missing.kdl"],
        &["config", "compose", "missing.kdl"],
    ] {
        let output = run_bounded(cli().current_dir(directory.path()).args(arguments));
        assert_eq!(output.status.code(), Some(3), "{arguments:?}");
        let message = stderr(&output);
        assert!(message.contains("missing."), "{message}");
        assert!(message.contains("No such file or directory"), "{message}");
    }
}

#[test]
fn an_existing_file_without_an_extension_is_the_path_shorthand() {
    let directory = TempDir::new().expect("directory");
    fs::write(directory.path().join("edge"), "not a configuration").expect("file");
    let output = run_bounded(cli().current_dir(directory.path()).arg("edge"));
    assert_ne!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!output.status.success());
}

#[test]
fn the_help_names_the_environment_variables_and_defaults_of_the_global_flags() {
    let output = run_bounded(cli().arg("--help"));
    let help = String::from_utf8_lossy(&output.stdout).into_owned();
    for text in [
        "[env: OXIROUTE_ENDPOINT]",
        "[env: OXIROUTE_MANAGEMENT_TOKEN_FILE]",
        "http://127.0.0.1:9900",
        "/etc/oxiroute/management.token",
        "[default: table]",
    ] {
        assert!(help.contains(text), "{text}: {help}");
    }
}

fn unused_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("port");
    format!("http://{}", listener.local_addr().expect("address"))
}

fn token_file(directory: &TempDir, name: &str, contents: &str, mode: u32) -> std::path::PathBuf {
    let path = directory.path().join(name);
    fs::write(&path, contents).expect("token");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
    path
}

#[test]
fn a_refused_connection_names_the_endpoint_and_how_to_change_it() {
    let endpoint = unused_endpoint();
    let output = run_bounded(cli().args(["ready", "--endpoint", &endpoint]));
    assert_eq!(output.status.code(), Some(4));
    let message = stderr(&output);
    assert!(
        message.starts_with("oxiroute: cannot connect to "),
        "{message}"
    );
    assert!(message.contains(&endpoint), "{message}");
    assert!(message.contains("Connection refused"), "{message}");
    assert!(message.contains("is the daemon running?"), "{message}");
    assert!(
        message.contains("--endpoint or OXIROUTE_ENDPOINT"),
        "{message}"
    );
}

#[test]
fn token_problems_name_the_file_and_the_requirement() {
    let directory = TempDir::new().expect("directory");
    let endpoint = unused_endpoint();
    let loose = token_file(&directory, "loose.token", &"a".repeat(40), 0o644);
    let short = token_file(&directory, "short.token", "tiny", 0o600);
    let missing = directory.path().join("missing.token");
    for (path, reason) in [
        (&loose, "mode 0644"),
        (&short, "token of 4 characters"),
        (&missing, "No such file or directory"),
    ] {
        let output = run_bounded(cli().args([
            "status",
            "--endpoint",
            &endpoint,
            "--token-file",
            path.to_str().unwrap(),
        ]));
        assert_eq!(output.status.code(), Some(3), "{reason}");
        let message = stderr(&output);
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains(reason), "{message}");
        assert!(message.contains("0400 or 0600"), "{message}");
    }
}

#[test]
fn quiet_silences_the_output_and_never_the_errors() {
    let directory = TempDir::new().expect("directory");
    let output = run_bounded(cli().current_dir(directory.path()).args([
        "--quiet",
        "config",
        "check",
        "missing.kdl",
    ]));
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).starts_with("oxiroute: "),
        "{}",
        stderr(&output)
    );

    let output = run_bounded(cli().args(["--quiet", "ready", "--endpoint", &unused_endpoint()]));
    assert_eq!(output.status.code(), Some(4));
    assert!(
        stderr(&output).contains("cannot connect"),
        "{}",
        stderr(&output)
    );
    assert!(output.stdout.is_empty());

    let output = run_bounded(cli().args(["version", "--quiet"]));
    assert!(output.status.success());
}

#[test]
fn every_failure_uses_the_same_stderr_prefix() {
    let directory = TempDir::new().expect("directory");
    let invalid = directory.path().join("invalid.kdl");
    fs::write(&invalid, "version 1\n(array)listeners {\n").expect("config");
    let endpoint = unused_endpoint();
    for arguments in [
        vec!["config", "check", "invalid.kdl"],
        vec!["config", "check", "missing.kdl"],
        vec!["serve", "missing.kdl"],
        vec!["import", "nginx", "missing.conf", "--emit", "preview"],
        vec!["ready", "--endpoint", endpoint.as_str()],
        vec!["rtmp", "stream", "disconnect", "stream-id"],
    ] {
        let output = run_bounded(cli().current_dir(directory.path()).args(&arguments));
        assert!(!output.status.success(), "{arguments:?}");
        let message = stderr(&output);
        assert!(
            message.starts_with("oxiroute: "),
            "{arguments:?}: {message}"
        );
        assert!(
            !message.contains("startup failed"),
            "{arguments:?}: {message}"
        );
    }
}

#[test]
fn a_closed_standard_output_ends_the_command_quietly() {
    let (reader, writer) = pipe().expect("pipe");
    drop(reader);
    let output = run_bounded(cli().arg("version").stdout(writer));
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn config_check_needs_no_management_token_and_names_the_variable_that_would_add_one() {
    let directory = TempDir::new().expect("directory");
    let config = directory.path().join("oxiroute.kdl");
    let bind = unused_endpoint().replace("http://", "");
    fs::write(
        &config,
        format!("version 1\n(object)management {{ bind \"{bind}\" }}\n"),
    )
    .expect("config");
    let check = |token: Option<&std::path::Path>| {
        let mut command = cli();
        command.env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE");
        if let Some(token) = token {
            command.env("OXIROUTE_MANAGEMENT_TOKEN_FILE", token);
        }
        run_bounded(command.args(["config", "check", config.to_str().unwrap()]))
    };

    let without = check(None);
    assert!(without.status.success(), "{}", stderr(&without));
    let report = String::from_utf8_lossy(&without.stdout).into_owned();
    assert!(report.contains("configuration is valid"), "{report}");
    assert!(
        report.contains("OXIROUTE_MANAGEMENT_TOKEN_FILE"),
        "{report}"
    );

    let good = token_file(&directory, "good.token", &"b".repeat(40), 0o600);
    let with = check(Some(&good));
    assert!(with.status.success(), "{}", stderr(&with));
    assert!(!String::from_utf8_lossy(&with.stdout).contains("OXIROUTE_MANAGEMENT_TOKEN_FILE"));

    let loose = token_file(&directory, "loose.token", &"b".repeat(40), 0o644);
    let refused = check(Some(&loose));
    assert_eq!(refused.status.code(), Some(3));
    let message = stderr(&refused);
    assert!(
        message.contains("loose.token") && message.contains("0400 or 0600"),
        "{message}"
    );
}
