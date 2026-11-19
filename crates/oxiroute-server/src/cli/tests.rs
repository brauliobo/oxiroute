use std::path::{Path, PathBuf};

use clap::Parser as _;

use super::{
    Cli, Command,
    config::{ComposeFormat, ConfigCommand},
    global::Output,
    pool::PoolCommand,
    process::ProcessCommand,
};

fn serve_config(arguments: &[&str]) -> PathBuf {
    let cli = Cli::try_parse_process_from(arguments).expect("serve");
    match cli.subcommand() {
        Command::Serve(args) => args.config.clone(),
        _ => panic!("serve command"),
    }
}

fn parse_error(arguments: &[&str]) -> clap::Error {
    Cli::try_parse_process_from(arguments)
        .err()
        .expect("a parse error")
}

#[test]
fn serve_takes_its_configuration_explicitly_or_as_a_path_shorthand() {
    let directory = tempfile::TempDir::new().expect("directory");
    let plain_file = directory.path().join("edge");
    std::fs::write(&plain_file, "version 1\n").expect("file");
    let plain_file = plain_file.display().to_string();
    for operand in [
        "edge.lua",
        "oxiroute.hocon",
        "./edge",
        "etc/edge",
        &plain_file,
    ] {
        assert_eq!(
            serve_config(&["oxiroute", operand]),
            PathBuf::from(operand),
            "{operand}"
        );
    }
    assert_eq!(
        serve_config(&["oxiroute", "serve", "edge.lua"]),
        PathBuf::from("edge.lua")
    );
    assert_eq!(
        serve_config(&["oxiroute", "serve"]),
        PathBuf::from("oxiroute.kdl")
    );
    assert_eq!(
        serve_config(&[
            "oxiroute",
            "--quiet",
            "--endpoint",
            "http://127.0.0.1:1",
            "edge.lua"
        ]),
        PathBuf::from("edge.lua")
    );
}

#[test]
fn a_mistyped_subcommand_is_reported_with_suggestions_and_never_served() {
    for (typo, suggestion) in [
        ("stauts", "status"),
        ("servr", "server"),
        ("complitions", "completions"),
    ] {
        let error = parse_error(&["oxiroute", typo]);
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::InvalidSubcommand,
            "{typo}"
        );
        if typo != "complitions" {
            assert!(error.to_string().contains(suggestion), "{typo}: {error}");
        }
    }
    assert_eq!(
        parse_error(&["oxiroute", "--quiet", "stauts"]).kind(),
        clap::error::ErrorKind::InvalidSubcommand
    );
}

#[test]
fn no_subcommand_prints_the_help() {
    assert_eq!(
        parse_error(&["oxiroute"]).kind(),
        clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
}

#[test]
fn global_flags_are_accepted_before_and_after_every_subcommand() {
    for tail in [
        &["status"][..],
        &["check"],
        &["top", "--once"],
        &["server", "list", "--pool", "farm"],
        &["pool", "drain", "farm"],
        &["generation", "reload"],
        &["config", "get"],
        &["version"],
    ] {
        let flags = [
            "--endpoint",
            "http://127.0.0.1:9",
            "--output",
            "json",
            "--quiet",
        ];
        let before =
            Cli::try_parse_process_from(["oxiroute"].iter().chain(flags.iter()).chain(tail.iter()))
                .expect("flags before");
        let after =
            Cli::try_parse_process_from(["oxiroute"].iter().chain(tail.iter()).chain(flags.iter()))
                .expect("flags after");
        for cli in [before, after] {
            assert_eq!(
                cli.global.endpoint.as_deref(),
                Some("http://127.0.0.1:9"),
                "{tail:?}"
            );
            assert!(matches!(cli.global.output, Output::Json), "{tail:?}");
            assert!(cli.global.quiet, "{tail:?}");
        }
    }
    let socket =
        Cli::try_parse_process_from(["oxiroute", "check", "--socket", "/run/oxiroute/status.sock"])
            .expect("socket after check");
    assert_eq!(
        socket.global.socket.as_deref(),
        Some(Path::new("/run/oxiroute/status.sock"))
    );
}

#[test]
fn upgrade_names_its_configuration_its_binary_and_whether_to_wait() {
    let named = Cli::try_parse_process_from([
        "oxiroute",
        "upgrade",
        "--config",
        "edge.kdl",
        "--binary",
        "/opt/oxiroute/bin/oxiroute",
        "--wait",
    ])
    .expect("upgrade");
    let Command::Upgrade(args) = named.subcommand() else {
        panic!("upgrade command");
    };
    assert_eq!(args.config, PathBuf::from("edge.kdl"));
    assert_eq!(
        args.binary,
        Some(PathBuf::from("/opt/oxiroute/bin/oxiroute"))
    );
    assert!(args.wait);

    let plain = Cli::try_parse_process_from(["oxiroute", "upgrade"]).expect("upgrade");
    let Command::Upgrade(args) = plain.subcommand() else {
        panic!("upgrade command");
    };
    assert_eq!(args.config, PathBuf::from("/etc/oxiroute/oxiroute.kdl"));
    assert_eq!(args.binary, None);
    assert!(!args.wait);
}

#[test]
fn offline_and_management_commands_parse_beside_serve() {
    let check = Cli::try_parse_process_from(["oxiroute", "config", "check", "edge.lua"])
        .expect("config check");
    let get = Cli::try_parse_process_from(["oxiroute", "config", "get"]).expect("config get");
    let status = Cli::try_parse_process_from(["oxiroute", "status"]).expect("status");

    assert!(matches!(
        check.subcommand(),
        Command::Config {
            command: ConfigCommand::Check { config }
        } if config == &PathBuf::from("edge.lua")
    ));
    assert!(matches!(
        get.subcommand(),
        Command::Config {
            command: ConfigCommand::Get
        }
    ));
    assert!(matches!(
        status.subcommand(),
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
        cli.subcommand(),
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
        command:
            PoolCommand::Evacuate {
                pools,
                force,
                persist,
            },
    } = cli.subcommand()
    else {
        panic!("pool evacuate command")
    };
    assert!(!force);
    assert!(!persist.persist);
    assert_eq!(*pools, ["gpu-render", "voice-tts"]);
}

#[test]
fn a_runtime_change_is_kept_in_the_overlay_only_when_asked() {
    let cli = Cli::try_parse_from(["oxiroute", "pool", "drain", "web", "--persist"]).expect("CLI");
    let Command::Pool {
        command: PoolCommand::Drain { persist, .. },
    } = cli.subcommand()
    else {
        panic!("pool drain command")
    };
    assert!(persist.persist);
}
