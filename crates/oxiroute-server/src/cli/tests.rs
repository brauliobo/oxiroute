use std::path::PathBuf;

use clap::Parser as _;

use super::{
    Cli, Command,
    config::{ComposeFormat, ConfigCommand},
    pool::PoolCommand,
    process::ProcessCommand,
};

fn serve_config(arguments: &[&str]) -> PathBuf {
    let cli = Cli::try_parse_process_from(arguments).expect("serve");
    match cli.command() {
        Command::Serve(args) => args.config.clone(),
        _ => panic!("serve command"),
    }
}

#[test]
fn a_lone_path_or_no_argument_serves_a_configuration() {
    assert_eq!(
        serve_config(&["oxiroute", "serve", "edge.lua"]),
        PathBuf::from("edge.lua")
    );
    assert_eq!(
        serve_config(&["oxiroute", "edge.lua"]),
        PathBuf::from("edge.lua")
    );
    assert_eq!(serve_config(&["oxiroute"]), PathBuf::from("oxiroute.kdl"));
}

#[test]
fn offline_and_management_commands_parse_beside_serve() {
    let check = Cli::try_parse_process_from(["oxiroute", "config", "check", "edge.lua"])
        .expect("config check");
    let get = Cli::try_parse_process_from(["oxiroute", "config", "get"]).expect("config get");
    let status = Cli::try_parse_process_from(["oxiroute", "status"]).expect("status");

    assert!(matches!(
        check.command(),
        Command::Config {
            command: ConfigCommand::Check { config }
        } if config == &PathBuf::from("edge.lua")
    ));
    assert!(matches!(
        get.command(),
        Command::Config {
            command: ConfigCommand::Get
        }
    ));
    assert!(matches!(
        status.command(),
        Command::Process(ProcessCommand::Status)
    ));
}

#[test]
fn parses_canonical_config_composition_inputs_in_order() {
    let cli = Cli::try_parse_process_from([
        "oxiroute",
        "config",
        "compose",
        "--format",
        "hocon",
        "nginx.lua",
        "haproxy.lua",
    ])
    .expect("config compose");

    assert!(matches!(
        cli.command(),
        Command::Config {
            command: ConfigCommand::Compose { format, configs }
        } if *format == ComposeFormat::Hocon
            && configs == &[PathBuf::from("nginx.lua"), PathBuf::from("haproxy.lua")]
    ));
}

#[test]
fn parses_pool_evacuate_without_force_by_default() {
    let cli = Cli::try_parse_from(["oxiroute", "pool", "evacuate", "gpu-render", "voice-tts"])
        .expect("CLI");
    let Command::Pool {
        command: PoolCommand::Evacuate { pools, force },
    } = cli.command()
    else {
        panic!("pool evacuate command")
    };
    assert!(!force);
    assert_eq!(*pools, ["gpu-render", "voice-tts"]);
}
