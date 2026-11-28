use std::{
    net::TcpListener,
    path::{Path, PathBuf},
};

use clap::Parser as _;
use oxiroute_config_source::ConfigFormat;

use super::{
    BUILD_VERSION, Cli, Command,
    config::{ComposeFormat, ConfigCommand, check as config_check},
    global::Output,
    net::Package,
    pool::PoolCommand,
    process::ProcessCommand,
    version::VersionArgs,
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
        "oxiroute.yaml",
        "oxiroute.yml",
        "oxiroute.toml",
        "oxiroute.oxi",
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
        &["reload"],
        &["reload", "--wait"],
        &["config", "get"],
        &["version"],
        &["version", "--verbose"],
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
            command: ConfigCommand::Check {
                config,
                input_format: None,
                no_probe: false,
                dump: false,
                format: None,
            }
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

    for (flag, expected) in [
        ("yaml", ComposeFormat::Yaml),
        ("toml", ComposeFormat::Toml),
        ("oxi", ComposeFormat::Oxi),
    ] {
        let cli = Cli::try_parse_process_from([
            "oxiroute",
            "config",
            "compose",
            "--format",
            flag,
            "edge.yaml",
            "edge.toml",
        ])
        .expect("config compose yaml/toml/oxi");

        assert!(matches!(
            cli.subcommand(),
            Command::Config {
                command: ConfigCommand::Compose { format, configs }
            } if *format == expected
                && configs == &[PathBuf::from("edge.yaml"), PathBuf::from("edge.toml")]
        ));
    }

    assert!(
        Cli::try_parse_process_from([
            "oxiroute", "config", "validate", "--format", "oxi", "edge.cfg",
        ])
        .is_ok()
    );
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

#[test]
fn config_check_accepts_test_alias_no_probe_dump_and_format() {
    let cli = Cli::try_parse_process_from([
        "oxiroute",
        "config",
        "check",
        "--no-probe",
        "--dump",
        "--format",
        "yaml",
        "edge.kdl",
    ])
    .expect("config check flags");
    assert!(matches!(
        cli.subcommand(),
        Command::Config {
            command: ConfigCommand::Check {
                config,
                input_format: None,
                no_probe: true,
                dump: true,
                format: Some(ComposeFormat::Yaml),
            }
        } if config == &PathBuf::from("edge.kdl")
    ));

    let alias = Cli::try_parse_process_from(["oxiroute", "config", "test", "edge.oxi"])
        .expect("config test alias");
    assert!(matches!(
        alias.subcommand(),
        Command::Config {
            command: ConfigCommand::Check {
                config,
                input_format: None,
                no_probe: false,
                dump: false,
                format: None,
            }
        } if config == &PathBuf::from("edge.oxi")
    ));

    let dump_default =
        Cli::try_parse_process_from(["oxiroute", "config", "test", "--dump", "edge.kdl"])
            .expect("config test --dump");
    assert!(matches!(
        dump_default.subcommand(),
        Command::Config {
            command: ConfigCommand::Check {
                dump: true,
                format: None,
                ..
            }
        }
    ));

    assert!(
        Cli::try_parse_process_from([
            "oxiroute", "config", "check", "--format", "yaml", "edge.kdl"
        ])
        .is_err(),
        "--format requires --dump"
    );
}

#[test]
fn config_check_report_supports_no_probe_and_dump_formats() {
    let directory = tempfile::TempDir::new().expect("directory");
    let occupied = TcpListener::bind("127.0.0.1:0").expect("bind occupied listener");
    let bind = occupied.local_addr().expect("occupied addr");
    let validated = serde_json::from_value::<oxiroute_config::ConfigDraft>(serde_json::json!({
        "version": 1,
        "listeners": [{
            "name": "web",
            "protocol": "http",
            "service": "web",
            "bind": { "type": "socket", "address": bind.to_string() },
        }],
        "upstream_pools": [{
            "name": "app",
            "servers": [{
                "name": "app-1",
                "endpoint": { "type": "socket", "address": "127.0.0.1:9" },
            }],
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": { "kind": "segment_prefix", "value": "/" },
                "action": { "type": "proxy", "upstream_pool": "app", "policy": {} },
            }],
        }],
    }))
    .expect("valid draft")
    .validate()
    .expect("validated config");
    let kdl_source =
        oxiroute_config_source::render_config(ConfigFormat::Kdl, &validated).expect("render kdl");
    let config_path = directory.path().join("edge.kdl");
    std::fs::write(&config_path, kdl_source).expect("write config");

    assert!(
        config_check::report(&config_path, true, None).is_err(),
        "probing an already-bound listener should fail"
    );

    let plain = config_check::report(&config_path, false, None).expect("check without probe");
    assert_eq!(plain, "configuration is valid and runtime-preparable\n");

    let yaml = config_check::report(&config_path, false, Some(ConfigFormat::Yaml))
        .expect("check with yaml dump");
    assert!(
        yaml.starts_with("configuration is valid and runtime-preparable\n"),
        "{yaml}"
    );
    assert!(yaml.contains("version: 1"), "{yaml}");
    assert!(yaml.contains(&bind.to_string()), "{yaml}");

    let kdl = config_check::report(&config_path, false, Some(ConfigFormat::Kdl))
        .expect("check with kdl dump");
    assert!(kdl.contains("version 1"), "{kdl}");
    assert!(kdl.contains(&bind.to_string()), "{kdl}");

    let oxi = config_check::report(&config_path, false, Some(ConfigFormat::Oxi))
        .expect("check with oxi dump");
    assert!(oxi.contains("version 1"), "{oxi}");
    assert!(oxi.contains(&bind.to_string()), "{oxi}");
}

#[test]
fn version_verbose_parses_short_and_long_and_renders_structured_output() {
    for args in [
        &["oxiroute", "version", "-v"][..],
        &["oxiroute", "version", "--verbose"],
    ] {
        let cli = Cli::try_parse_process_from(args).expect("version verbose");
        assert!(matches!(
            cli.subcommand(),
            Command::Version(VersionArgs { verbose: true })
        ));
    }

    let plain = VersionArgs { verbose: false }
        .render(Output::Table)
        .expect("plain version");
    assert_eq!(plain, format!("{BUILD_VERSION}\n"));

    let table = VersionArgs { verbose: true }
        .render(Output::Table)
        .expect("verbose table");
    let expected_target = format!("{}/{}", std::env::consts::ARCH, std::env::consts::OS);
    let expected_cpu = oxiroute_cpu::Level::hardware().name();
    for line in [
        format!("version: {BUILD_VERSION}"),
        format!("target: {expected_target}"),
        "formats: oxi, kdl, yaml, toml, lua, hocon, uci".to_owned(),
        "protocols: http/1.1, http/2, http/3, forward-http1, forward-http2, forward-http3, rtmp, tcp, udp"
            .to_owned(),
        "tls_backends: openssl, rustls".to_owned(),
        "data_planes: tokio, epoll".to_owned(),
        format!("cpu_level: {expected_cpu}"),
    ] {
        assert!(table.contains(&line), "missing `{line}` in:\n{table}");
    }

    let json_text = VersionArgs { verbose: true }
        .render(Output::Json)
        .expect("verbose json");
    let parsed: serde_json::Value = serde_json::from_str(&json_text).expect("valid JSON");
    assert_eq!(parsed["version"], BUILD_VERSION);
    assert_eq!(parsed["target"], expected_target);
    assert_eq!(parsed["cpu_level"], expected_cpu);
    assert_eq!(
        parsed["formats"],
        serde_json::json!(["oxi", "kdl", "yaml", "toml", "lua", "hocon", "uci"])
    );
    assert_eq!(
        parsed["protocols"],
        serde_json::json!([
            "http/1.1",
            "http/2",
            "http/3",
            "forward-http1",
            "forward-http2",
            "forward-http3",
            "rtmp",
            "tcp",
            "udp"
        ])
    );
    assert_eq!(
        parsed["tls_backends"],
        serde_json::json!(["openssl", "rustls"])
    );
    assert_eq!(parsed["data_planes"], serde_json::json!(["tokio", "epoll"]));
}

#[test]
fn top_level_reload_parses_with_and_without_wait() {
    let now = Cli::try_parse_process_from(["oxiroute", "reload"]).expect("reload");
    assert!(matches!(
        now.subcommand(),
        Command::Process(ProcessCommand::Reload { wait: false })
    ));

    let waiting =
        Cli::try_parse_process_from(["oxiroute", "reload", "--wait"]).expect("reload --wait");
    assert!(matches!(
        waiting.subcommand(),
        Command::Process(ProcessCommand::Reload { wait: true })
    ));
}

#[test]
fn package_discovery_supports_all_canonical_formats_in_priority_order() {
    let installed = Package::installed();
    let paths: Vec<&str> = installed
        .config_paths()
        .iter()
        .map(|path| path.to_str().expect("UTF-8 path"))
        .collect();
    assert_eq!(
        paths,
        [
            "/etc/oxiroute/oxiroute.oxi",
            "/etc/oxiroute/oxiroute.kdl",
            "/etc/oxiroute/oxiroute.kdl2",
            "/etc/oxiroute/oxiroute.yaml",
            "/etc/oxiroute/oxiroute.yml",
            "/etc/oxiroute/oxiroute.toml",
            "/etc/oxiroute/oxiroute.lua",
            "/etc/oxiroute/oxiroute.conf",
            "/etc/oxiroute/oxiroute.hocon",
            "/etc/oxiroute/oxiroute.uci",
        ]
    );

    let names = [
        "oxiroute.oxi",
        "oxiroute.kdl",
        "oxiroute.kdl2",
        "oxiroute.yaml",
        "oxiroute.yml",
        "oxiroute.toml",
        "oxiroute.lua",
        "oxiroute.conf",
        "oxiroute.hocon",
        "oxiroute.uci",
    ];
    for (name, body, expected) in [
        (
            "oxiroute.yaml",
            "version: 1\nmanagement:\n  bind: 127.0.0.1:9101\n",
            "http://127.0.0.1:9101",
        ),
        (
            "oxiroute.yml",
            "version: 1\nmanagement:\n  bind: 127.0.0.1:9102\n",
            "http://127.0.0.1:9102",
        ),
        (
            "oxiroute.toml",
            "version = 1\n[management]\nbind = \"127.0.0.1:9103\"\n",
            "http://127.0.0.1:9103",
        ),
        (
            "oxiroute.conf",
            "version = 1\nmanagement { bind = \"127.0.0.1:9104\" }\n",
            "http://127.0.0.1:9104",
        ),
        (
            "oxiroute.oxi",
            "version 1\nmanagement\n  bind \"127.0.0.1:9105\"\n",
            "http://127.0.0.1:9105",
        ),
    ] {
        let directory = tempfile::TempDir::new().expect("directory");
        std::fs::write(directory.path().join(name), body).expect("write packaged config");
        let package = Package::at(
            directory.path().join("oxiroute.env"),
            names
                .iter()
                .map(|candidate| directory.path().join(candidate))
                .collect(),
            directory.path().join("management.token"),
        );
        assert_eq!(
            package.discovered_endpoint().as_deref(),
            Some(expected),
            "{name}"
        );
    }
}
