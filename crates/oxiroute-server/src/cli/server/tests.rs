use std::{path::Path, time::Duration};

use clap::Parser as _;
use serde_json::json;

use super::args::ServerCommand;
use crate::cli::{Cli, Command, global::Output};

fn server(arguments: &[&str]) -> Cli {
    Cli::try_parse_from(
        ["oxiroute", "server"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
    .expect("CLI")
}

fn command(cli: &Cli) -> &ServerCommand {
    match cli.command() {
        Command::Server { command } => command,
        _ => panic!("server command"),
    }
}

#[test]
fn parses_repeated_pool_targets_and_stable_global_options() {
    let cli = Cli::try_parse_from([
        "oxiroute",
        "--endpoint",
        "http://127.0.0.1:9900",
        "--output",
        "json",
        "server",
        "drain",
        "--pool",
        "public-v4",
        "--pool",
        "public-v6",
        "origin-a",
    ])
    .expect("CLI");
    let ServerCommand::Drain(target) = command(&cli) else {
        panic!("server drain command")
    };
    assert_eq!(target.pools, ["public-v4", "public-v6"]);
    assert_eq!(target.server, "origin-a");
    assert!(matches!(cli.global.output, Output::Json));
}

#[test]
fn rejects_empty_batch_targets() {
    assert!(Cli::try_parse_from(["oxiroute", "pool", "drain"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "listener", "ready", ""]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "server", "show", "--pool", "", "origin"]).is_err());
}

#[test]
fn parses_registration_commands_and_socket_endpoints() {
    let cli = Cli::try_parse_from([
        "oxiroute",
        "--socket",
        "/run/oxiroute/register.sock",
        "server",
        "register",
        "--pool",
        "voice",
        "tts-1",
        "--address",
        "192.0.2.10:8000",
        "--ttl",
        "15s",
        "--label",
        "gpu=0",
        "--heartbeat",
    ])
    .expect("CLI");
    let ServerCommand::Register(args) = command(&cli) else {
        panic!("server register command")
    };
    assert_eq!(args.ttl, Some(Duration::from_secs(15)));
    assert_eq!(args.labels, [("gpu".to_owned(), "0".to_owned())]);
    assert!(args.heartbeat);
    assert_eq!(
        cli.global.socket.as_deref(),
        Some(Path::new("/run/oxiroute/register.sock"))
    );
}

#[test]
fn parses_server_evacuate_with_force_and_repeated_pools() {
    let cli = server(&[
        "evacuate",
        "--force",
        "--pool",
        "gpu-render",
        "--pool",
        "voice-tts",
        "node-a",
    ]);
    let ServerCommand::Evacuate { target, force } = command(&cli) else {
        panic!("server evacuate command")
    };
    assert!(*force);
    assert_eq!(target.pools, ["gpu-render", "voice-tts"]);
    assert_eq!(target.server, "node-a");
}

#[test]
fn evacuate_requires_its_targets() {
    assert!(Cli::try_parse_from(["oxiroute", "pool", "evacuate"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "pool", "evacuate", ""]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "server", "evacuate", "node-a"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "server", "evacuate", "--pool", "gpu"]).is_err());
}

#[test]
fn evacuate_request_bodies_name_the_targets_and_the_force_flag() {
    let cli = server(&[
        "evacuate",
        "--pool",
        "gpu-render",
        "--pool",
        "voice-tts",
        "node-a",
    ]);
    let ServerCommand::Evacuate { target, .. } = command(&cli) else {
        panic!("server evacuate command")
    };

    assert_eq!(
        target.body(json!({ "force": true })),
        json!({
            "targets": [
                { "pool": "gpu-render", "server": "node-a" },
                { "pool": "voice-tts", "server": "node-a" },
            ],
            "force": true,
        })
    );
}
