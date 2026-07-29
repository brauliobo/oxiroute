use std::{
    fs,
    io::{Read as _, Write as _},
    net::TcpListener,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    process::Command,
    thread,
};

use tempfile::TempDir;

use oxiroute_config::Config;
use oxiroute_config_source::{
    ConfigFormat, decode_value, render_config, resolve_source_with_format,
};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

#[test]
fn json_output_is_script_safe_and_preserves_fields() {
    let endpoint = serve_once(200, r#"{"schemaVersion":1,"ready":true}"#);
    let output = cli()
        .args(["--endpoint", &endpoint, "--output", "json", "ready"])
        .output()
        .expect("CLI");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"ready\":true,\"schemaVersion\":1}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn endpoint_failure_and_remote_status_use_stable_exit_categories() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve endpoint");
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let unavailable = cli()
        .args(["--endpoint", &endpoint, "status"])
        .output()
        .expect("CLI");
    assert_eq!(unavailable.status.code(), Some(4));

    let endpoint = serve_once(404, r#"{"error":{"code":"server_not_found"}}"#);
    let missing = cli()
        .args(["--endpoint", &endpoint, "ready"])
        .output()
        .expect("CLI");
    assert_eq!(
        missing.status.code(),
        Some(6),
        "stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
fn authentication_failure_does_not_expose_token() {
    let directory = TempDir::new().expect("directory");
    let token_path = directory.path().join("management.token");
    fs::write(&token_path, format!("{TOKEN}\n")).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let endpoint = serve_once(401, r#"{"error":{"code":"unauthorized"}}"#);
    let output = cli()
        .args([
            "--endpoint",
            &endpoint,
            "--token-file",
            token_path.to_str().unwrap(),
            "server",
            "list",
        ])
        .output()
        .expect("CLI");

    assert_eq!(
        output.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    assert!(output.stdout.is_empty());
}

#[test]
fn unowned_rtmp_controls_are_explicitly_unsupported() {
    let output = cli()
        .args(["rtmp", "stream", "disconnect", "stream-id"])
        .output()
        .expect("CLI");
    assert_eq!(output.status.code(), Some(9));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported"));
}

#[test]
fn token_file_reads_are_bounded_and_do_not_follow_symlinks() {
    let directory = TempDir::new().expect("directory");
    let oversized = directory.path().join("oversized.token");
    fs::write(&oversized, "x".repeat(514)).expect("oversized token");
    fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).expect("mode");
    let oversized_output = cli()
        .args([
            "--token-file",
            oversized.to_str().unwrap(),
            "server",
            "list",
        ])
        .output()
        .expect("CLI");
    assert_eq!(oversized_output.status.code(), Some(3));

    let valid = directory.path().join("valid.token");
    fs::write(&valid, TOKEN).expect("valid token");
    fs::set_permissions(&valid, fs::Permissions::from_mode(0o600)).expect("mode");
    let link = directory.path().join("link.token");
    std::os::unix::fs::symlink(&valid, &link).expect("token symlink");
    let link_output = cli()
        .args(["--token-file", link.to_str().unwrap(), "server", "list"])
        .output()
        .expect("CLI");
    assert_eq!(link_output.status.code(), Some(3));
}

#[test]
fn chunked_interim_response_is_decoded_by_the_cli_process() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP endpoint");
    let address = listener.local_addr().expect("address");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request");
        read_request_head(&mut stream);
        stream
            .write_all(
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\ne\r\n{\"ready\":true}\r\n0\r\n\r\n",
            )
            .expect("response");
    });
    let endpoint = format!("http://{address}");

    let output = cli()
        .args(["--endpoint", &endpoint, "--output", "json", "ready"])
        .output()
        .expect("CLI");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"ready\":true}\n"
    );
}

#[test]
fn config_check_accepts_all_source_formats_and_compose_defaults_to_kdl() {
    let directory = TempDir::new().expect("directory");
    let config = empty_config();
    for (extension, format) in [
        ("kdl", ConfigFormat::Kdl),
        ("lua", ConfigFormat::Lua),
        ("uci", ConfigFormat::Uci),
        ("hocon", ConfigFormat::Hocon),
    ] {
        let path = directory.path().join(format!("oxiroute.{extension}"));
        fs::write(&path, render_config(format, &config).expect("render")).expect("source");
        let output = cli()
            .args(["config", "check", path.to_str().unwrap()])
            .output()
            .expect("config check");
        assert!(
            output.status.success(),
            "{format:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let lua_path = directory.path().join("oxiroute.lua");
    let composed = cli()
        .args(["config", "compose", lua_path.to_str().unwrap()])
        .output()
        .expect("config compose");
    assert!(composed.status.success());
    assert!(decode_value(ConfigFormat::Kdl, &composed.stdout).is_ok());

    let lua = cli()
        .args([
            "config",
            "compose",
            "--format",
            "lua",
            lua_path.to_str().unwrap(),
        ])
        .output()
        .expect("Lua compose");
    assert!(lua.status.success());
    assert!(
        String::from_utf8(lua.stdout)
            .unwrap()
            .starts_with("return {")
    );
}

#[test]
fn import_previews_default_to_kdl_and_round_trip_every_format() {
    let directory = TempDir::new().expect("directory");
    let nginx_path = directory.path().join("nginx.conf");
    fs::write(
        &nginx_path,
        r"http {
          access_log off;
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend { server 127.0.0.1:8080; }
          server {
            listen 127.0.0.1:8088 default_server;
            server_name proxy.example;
            location / { proxy_pass http://backend; }
          }
        }",
    )
    .expect("nginx source");
    let haproxy_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/minimal-representable.cfg");
    let nginx_args = ["import", "nginx", nginx_path.to_str().unwrap()];
    let haproxy_args = ["import", "haproxy", haproxy_path.to_str().unwrap()];

    let default_nginx = import_preview(&nginx_args, None, ConfigFormat::Kdl);
    let default_haproxy = import_preview(&haproxy_args, None, ConfigFormat::Kdl);
    assert_eq!(default_nginx.listeners.len(), 1);
    assert_eq!(default_haproxy.listeners.len(), 1);

    for (name, format) in [
        ("kdl", ConfigFormat::Kdl),
        ("lua", ConfigFormat::Lua),
        ("uci", ConfigFormat::Uci),
        ("hocon", ConfigFormat::Hocon),
    ] {
        assert_eq!(
            import_preview(&nginx_args, Some(name), format),
            default_nginx,
            "nginx {name} preview"
        );
        assert_eq!(
            import_preview(&haproxy_args, Some(name), format),
            default_haproxy,
            "HAProxy {name} preview"
        );
    }
}

#[test]
fn import_report_output_is_unchanged_by_preview_format() {
    let haproxy_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/minimal-representable.cfg");
    let path = haproxy_path.to_str().unwrap();
    let report = cli()
        .args(["import", "haproxy", path])
        .output()
        .expect("default report");
    let formatted_report = cli()
        .args(["import", "haproxy", path, "--format", "lua"])
        .output()
        .expect("formatted report");

    assert!(report.status.success());
    assert!(formatted_report.status.success());
    assert_eq!(formatted_report.stdout, report.stdout);
    assert_eq!(formatted_report.stderr, report.stderr);
}

fn empty_config() -> Config {
    Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

fn import_preview(args: &[&str], format_name: Option<&str>, format: ConfigFormat) -> Config {
    let mut command = cli();
    command.args(args).args(["--output", "preview"]);
    if let Some(format_name) = format_name {
        command.args(["--format", format_name]);
    }
    let output = command.output().expect("import preview");
    assert!(
        output.status.success(),
        "{format:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if format == ConfigFormat::Lua {
        assert!(output.stdout.starts_with(b"return {"));
    }
    resolve_source_with_format(Path::new("preview"), &output.stdout, format)
        .expect("preview round trip")
        .config
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_oxiroute"))
}

fn serve_once(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP endpoint");
    let address = listener.local_addr().expect("address");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request");
        read_request_head(&mut stream);
        write!(
            stream,
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("response");
    });
    format!("http://{address}")
}

fn read_request_head(stream: &mut std::net::TcpStream) {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("read request");
        request.push(byte[0]);
    }
}
