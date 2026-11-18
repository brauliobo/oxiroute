use std::path::{Path, PathBuf};

use oxiroute_config_source::ConfigFormat;

use super::args::{ImportCommand, ImportOutput};
use crate::cli::{Cli, Command, config::ComposeFormat};

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_process_from(
        ["oxiroute", "import"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
}

fn parsed(arguments: &[&str]) -> Cli {
    parse(arguments).expect("import arguments")
}

fn import(cli: &Cli) -> &ImportCommand {
    match cli.command() {
        Command::Import { command } => command,
        _ => panic!("import command"),
    }
}

fn haproxy_fixture() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/synthetic/haproxy/env-health-routes.cfg")
        .display()
        .to_string()
}

#[test]
fn parses_explicit_haproxy_preprocessing_environment() {
    let cli = parsed(&[
        "haproxy",
        "/etc/haproxy/haproxy.cfg",
        "--node-ip",
        "192.0.2.37",
        "--gpu1-defined",
        "--shadow-port-offset",
        "10000",
        "--format",
        "hocon",
        "--output",
        "preview",
    ]);
    let ImportCommand::Haproxy {
        configs,
        node_ip,
        gpu1_defined,
        emit,
    } = import(&cli)
    else {
        panic!("HAProxy import command")
    };
    assert_eq!(configs, &[PathBuf::from("/etc/haproxy/haproxy.cfg")]);
    assert_eq!(*node_ip, Some("192.0.2.37".parse().unwrap()));
    assert!(*gpu1_defined);
    assert_eq!(emit.shadow_port_offset, Some(10_000));
    assert_eq!(emit.format, ComposeFormat::Hocon);
    assert_eq!(emit.output, ImportOutput::Preview);
}

#[test]
fn parses_nginx_timezone_and_shadow_inputs() {
    let cli = parsed(&[
        "nginx",
        "/etc/nginx/nginx.conf",
        "--host-timezone",
        "America/Argentina/Buenos_Aires",
        "--default-access-log-file",
        "/var/lib/oxiroute/http-access.jsonl",
        "--recording-root",
        "/mnt/cloud/4tb/cam-rtmp",
        "--default-error-server",
        "nginx/1.30.2",
        "--shadow-port-offset",
        "10000",
        "--format",
        "lua",
        "--output",
        "preview",
    ]);
    let ImportCommand::Nginx {
        host_timezone: Some(timezone),
        default_access_log_file: Some(path),
        recording_root: Some(recording_root),
        default_error_server: Some(server),
        emit,
        ..
    } = import(&cli)
    else {
        panic!("nginx import command")
    };
    assert_eq!(timezone, "America/Argentina/Buenos_Aires");
    assert_eq!(path, Path::new("/var/lib/oxiroute/http-access.jsonl"));
    assert_eq!(recording_root, Path::new("/mnt/cloud/4tb/cam-rtmp"));
    assert_eq!(server, "nginx/1.30.2");
    assert_eq!(emit.shadow_port_offset, Some(10_000));
    assert_eq!(emit.format, ComposeFormat::Lua);
    assert_eq!(emit.output, ImportOutput::Preview);
}

#[test]
fn haproxy_gpu_presence_requires_a_node_ip() {
    assert!(parse(&["haproxy", "/etc/haproxy/haproxy.cfg", "--gpu1-defined"]).is_err());
}

#[test]
fn haproxy_shadow_preview_shifts_only_listener_ports() {
    let fixture = haproxy_fixture();
    let cli = parsed(&[
        "haproxy",
        &fixture,
        "--node-ip",
        "192.0.2.15",
        "--gpu1-defined",
        "--shadow-port-offset",
        "10000",
        "--format",
        "hocon",
        "--output",
        "preview",
    ]);
    let output = import(&cli).render().expect("shadow preview");

    for listener in ["192.0.2.15:21434", "192.0.2.15:21435", "192.0.2.15:21436"] {
        assert!(output.contains(listener), "missing {listener}");
    }
    assert!(output.contains("127.0.0.1:3000"));
    assert!(output.contains("127.0.0.1:3001"));
    assert!(oxiroute_config_source::decode_value(ConfigFormat::Hocon, output.as_bytes()).is_ok());
}

#[test]
fn haproxy_shadow_preview_rejects_port_overflow_and_report_mode() {
    let fixture = haproxy_fixture();
    let render = |offset: &str, output: &str| {
        let cli = parsed(&[
            "haproxy",
            &fixture,
            "--node-ip",
            "192.0.2.15",
            "--gpu1-defined",
            "--shadow-port-offset",
            offset,
            "--output",
            output,
        ]);
        import(&cli).render().is_err()
    };

    assert!(render("60000", "preview"));
    assert!(render("10000", "report"));
}
