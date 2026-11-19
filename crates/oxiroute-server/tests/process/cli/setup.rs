//! `oxiroute init` and `token` against real files and a real daemon.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;

use super::{Daemon, cli_command, reserve_tcp_address, stderr, stdout};

fn run(words: &[&str]) -> std::process::Output {
    cli_command().args(words).output().expect("oxiroute CLI")
}

fn run_in(home: &std::path::Path, words: &[&str]) -> std::process::Output {
    cli_command()
        .env_remove("OXIROUTE_CONTEXT")
        .env("XDG_CONFIG_HOME", home)
        .args(words)
        .output()
        .expect("oxiroute CLI")
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

#[test]
fn init_then_token_create_gives_a_configuration_that_checks_clean() {
    let directory = TempDir::new().expect("directory");
    let config = directory.path().join("oxiroute.kdl");
    let token = directory.path().join("management.token");
    let (config_arg, token_arg) = (
        config.to_str().expect("path"),
        token.to_str().expect("path"),
    );

    let written = run(&["init", "proxy", "--file", config_arg]);
    assert!(written.status.success(), "{}", stderr(&written));
    assert!(
        stdout(&written).contains("oxiroute token create"),
        "{}",
        stdout(&written)
    );
    let ports = fs::read_to_string(&config)
        .expect("configuration")
        .replace("0.0.0.0:8080", &reserve_tcp_address().to_string())
        .replace("127.0.0.1:9900", &reserve_tcp_address().to_string());
    fs::write(&config, ports).expect("configuration");
    let early = run(&["config", "check", config_arg]);
    assert_eq!(early.status.code(), Some(3), "{}", stderr(&early));

    let created = run(&["token", "create", "--token-file", token_arg]);
    assert!(created.status.success(), "{}", stderr(&created));
    assert_eq!(mode(&token), 0o600);
    assert!(!stdout(&created).contains(&fs::read_to_string(&token).expect("token")));
    let checked = run(&["config", "check", config_arg]);
    assert!(checked.status.success(), "{}", stderr(&checked));

    let again = run(&["init", "proxy", "--file", config_arg]);
    assert_eq!(again.status.code(), Some(3), "{}", stderr(&again));
    assert!(stderr(&again).contains("--force"), "{}", stderr(&again));
    assert!(
        run(&["init", "proxy", "--file", config_arg, "--force"])
            .status
            .success()
    );
    let twice = run(&["token", "create", "--token-file", token_arg]);
    assert_eq!(twice.status.code(), Some(3), "{}", stderr(&twice));
}

#[test]
fn token_create_writes_where_the_selected_context_reads_its_token() {
    let directory = TempDir::new().expect("directory");
    let home = directory.path().join("home");
    let token = directory.path().join("prod.token");
    let added = run_in(
        &home,
        &[
            "context",
            "add",
            "prod",
            "--address",
            "http://192.0.2.10:9900",
            "--token-from-file",
            token.to_str().expect("path"),
            "--use",
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));

    let created = run_in(&home, &["token", "create"]);
    assert!(created.status.success(), "{}", stderr(&created));
    assert_eq!(mode(&token), 0o600);
    assert_eq!(fs::read_to_string(&token).expect("token").trim().len(), 64);
}

#[test]
fn every_kind_and_format_prints_a_configuration() {
    for (kind, format, marker) in [
        ("static", "lua", "static_files"),
        ("rtmp", "kdl", "rtmp_services"),
        ("proxy", "hocon", "upstream_pools"),
    ] {
        let printed = run(&["init", kind, "--format", format, "--file", "-"]);
        assert!(printed.status.success(), "{kind}: {}", stderr(&printed));
        assert!(
            stdout(&printed).contains(marker),
            "{kind}: {}",
            stdout(&printed)
        );
    }
}

#[tokio::test]
async fn rotating_the_token_changes_what_the_cli_sends_and_the_daemon_takes_the_new_one_on_reload()
{
    let daemon = Daemon::start().await;
    let before = fs::read_to_string(&daemon.token_file).expect("token");
    assert!(daemon.run(&["status"]).status.success());

    let rotated = daemon.run(&["token", "rotate"]);
    assert!(rotated.status.success(), "{}", stderr(&rotated));
    let after = fs::read_to_string(&daemon.token_file).expect("token");
    assert_ne!(before, after);
    assert_eq!(after.len(), 64);
    assert_eq!(mode(&daemon.token_file), 0o600);

    let stale = daemon.run(&["status"]);
    assert_eq!(stale.status.code(), Some(5), "{}", stderr(&stale));

    let pid = i32::try_from(daemon.server.pid()).expect("pid");
    kill_process(Pid::from_raw(pid).expect("pid"), Signal::HUP).expect("reload signal");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !daemon.run(&["status"]).status.success() {
        assert!(
            Instant::now() < deadline,
            "the reload did not take the new token"
        );
        tokio::task::yield_now().await;
    }
}
