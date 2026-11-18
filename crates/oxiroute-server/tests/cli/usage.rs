//! Command-line parsing seen from the process: mistyped words, the path shorthand and unreadable files.

#[path = "../cli_support/mod.rs"]
mod cli_support;

use std::{
    fs,
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
