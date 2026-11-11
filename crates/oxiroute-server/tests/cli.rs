mod cli_support;
#[path = "support/ports.rs"]
mod ports_support;
use std::{
    fs,
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt as _,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use cli_support::{cli, output_text};

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{ConfigFormat, decode_value, render_config};
use serde_json::Value;

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
        "{\"schemaVersion\":1,\"ready\":true}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn json_output_prints_the_server_body_untouched() {
    let body = r#"{"servers":[{"pool":"b","name":"é\u0000","n":1.50,"x":1e2},{"pool":"a"}]}"#;
    let endpoint = serve_once(200, body);
    let output = cli()
        .args(["--endpoint", &endpoint, "--output", "json", "ready"])
        .output()
        .expect("CLI");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{body}\n")
    );
}

#[test]
fn json_output_rejects_a_body_that_is_not_json() {
    let endpoint = serve_once(200, "{\"ready\":true}\u{1b}[31m");
    let output = cli()
        .args(["--endpoint", &endpoint, "--output", "json", "ready"])
        .output()
        .expect("CLI");

    assert_eq!(output.status.code(), Some(8));
    assert!(output.stdout.is_empty());
}

#[test]
fn quiet_json_output_prints_nothing() {
    let endpoint = serve_once(200, r#"{"ready":true}"#);
    let output = cli()
        .args([
            "--endpoint",
            &endpoint,
            "--output",
            "json",
            "--quiet",
            "ready",
        ])
        .output()
        .expect("CLI");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn public_ready_does_not_read_an_unusable_token_file() {
    let directory = TempDir::new().expect("directory");
    let missing_token = directory.path().join("missing.token");
    let endpoint = serve_once(200, r#"{"schemaVersion":1,"ready":true}"#);
    let output = cli()
        .args([
            "--endpoint",
            &endpoint,
            "--output",
            "json",
            "--token-file",
            missing_token.to_str().unwrap(),
            "ready",
        ])
        .output()
        .expect("CLI");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"schemaVersion\":1,\"ready\":true}\n"
    );
}

#[test]
fn configured_environment_token_path_is_used_without_the_cli_option() {
    let directory = TempDir::new().expect("directory");
    let token_path = directory.path().join("management.token");
    fs::write(&token_path, TOKEN).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let endpoint = serve_once(200, r#"{"status":"ok"}"#);
    let output = cli()
        .env("OXIROUTE_MANAGEMENT_TOKEN_FILE", &token_path)
        .args(["--endpoint", &endpoint, "--output", "json", "status"])
        .output()
        .expect("CLI");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"status\":\"ok\"}\n"
    );
}

#[test]
fn endpoint_failure_and_remote_status_use_stable_exit_categories() {
    let directory = TempDir::new().expect("directory");
    let token_path = directory.path().join("management.token");
    fs::write(&token_path, TOKEN).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let endpoint = format!("http://{}", ports_support::unused_address());
    let unavailable = cli()
        .args([
            "--endpoint",
            &endpoint,
            "--token-file",
            token_path.to_str().unwrap(),
            "status",
        ])
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
fn config_check_names_what_is_wrong_with_a_rejected_file() {
    let directory = TempDir::new().expect("directory");
    let check = |name: &str, source: &str| {
        let path = directory.path().join(name);
        fs::write(&path, source).expect("source");
        let output = cli()
            .args(["config", "check", path.to_str().unwrap()])
            .output()
            .expect("config check");
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let no_version = check("no-version.kdl", "(array)listeners {\n}\n");
    assert!(no_version.contains("E_INVALID_VALUE"), "{no_version}");
    assert!(no_version.contains("`version`"), "{no_version}");
    assert!(no_version.contains("version 1"), "{no_version}");
    assert!(!no_version.contains("canonical configuration was rejected\n"));

    let syntax = check("broken.kdl", "version 1\n(array)listeners {\n");
    assert!(syntax.contains("E_SYNTAX"), "{syntax}");
    assert!(syntax.contains("(line "), "{syntax}");
}

#[test]
fn http_422_names_the_failing_setting_and_reason() {
    let directory = TempDir::new().expect("directory");
    let token_path = directory.path().join("management.token");
    fs::write(&token_path, format!("{TOKEN}\n")).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let endpoint = serve_once(
        422,
        r#"{"diagnostics":[{"code":"E_INVALID_VALUE","severity":"error","stage":"validation","message":"configuration values, composition, or references are invalid","path":"/upstream_pools/farm","detail":"upstream pool `farm` contains duplicate endpoint `127.0.0.1:8080`"}]}"#,
    );
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

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E_INVALID_VALUE"), "{stderr}");
    assert!(stderr.contains("/upstream_pools/farm"), "{stderr}");
    assert!(stderr.contains("duplicate endpoint"), "{stderr}");
}

#[test]
fn config_check_accepts_all_source_formats_and_compose_defaults_to_kdl() {
    let directory = TempDir::new().expect("directory");
    let config = empty_config().validate().expect("valid empty config");
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
fn generation_reload_cli_re_resolves_a_deleted_nginx_site() {
    let server = NativeReloadServer::start(false);
    let initial = server.generation_status();
    assert_eq!(
        server
            .config()
            .pointer("/config/listeners")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    fs::remove_file(&server.site_path).expect("delete nginx site");
    let reload = server.run(&["generation", "reload"]);
    assert!(reload.status.success(), "{}", output_text(&reload));
    let candidate_revision = serde_json::from_slice::<Value>(&reload.stdout)
        .expect("reload response")
        .get("candidateRevision")
        .and_then(Value::as_str)
        .expect("candidate revision")
        .to_owned();
    let active = server.wait_for_active_change(
        initial["generation"]["activeRevision"]
            .as_str()
            .expect("initial active revision"),
    );

    assert_eq!(active["generation"]["activeRevision"], candidate_revision);
    assert_eq!(
        active["generation"]["diskRevision"],
        initial["generation"]["diskRevision"]
    );
    assert!(
        server
            .config()
            .pointer("/config/listeners")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    );
}

#[test]
fn generation_reload_cli_keeps_the_old_generation_and_surfaces_native_failure() {
    let server = NativeReloadServer::start(true);
    let initial = server.generation_status();
    let initial_revision = initial["generation"]["activeRevision"]
        .as_str()
        .expect("initial active revision")
        .to_owned();

    fs::remove_file(&server.site_path).expect("delete exact nginx include");
    let reload = server.run(&["generation", "reload"]);

    assert!(!reload.status.success());
    assert!(String::from_utf8_lossy(&reload.stderr).contains("E_NATIVE_SOURCE"));
    assert_eq!(
        server.generation_status()["generation"]["activeRevision"],
        initial_revision
    );
}

fn empty_config() -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        worker_scaling: None,
        min_active_workers: None,
        target_busy_percent: None,
        scale_down_delay_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
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
        audit: None,
        log_filter: None,
    }
}

struct NativeReloadServer {
    child: Child,
    endpoint: String,
    token_path: std::path::PathBuf,
    site_path: std::path::PathBuf,
    _directory: TempDir,
}

impl NativeReloadServer {
    fn start(exact_include: bool) -> Self {
        let directory = TempDir::new().expect("native reload directory");
        let native = directory.path().join("native");
        let sites = native.join("sites-enabled");
        fs::create_dir_all(&sites).expect("native directories");
        let management = reserve_tcp_address();
        let listener = reserve_tcp_address();
        let token_path = directory.path().join("management.token");
        fs::write(&token_path, TOKEN).expect("management token");
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))
            .expect("management token mode");
        let config_path = directory.path().join("oxiroute.kdl");
        fs::write(
            &config_path,
            format!(
                "version 1\n(object)management {{ bind \"{management}\" }}\nnginx_server \"native/nginx.conf\" {{ root_prefix \"native\" }}\n"
            ),
        )
        .expect("canonical source");
        fs::write(
            native.join("nginx.conf"),
            format!(
                "events {{}} http {{ access_log off; include sites-enabled/{}; }}",
                if exact_include { "site.conf" } else { "*.conf" }
            ),
        )
        .expect("nginx root");
        let site_path = sites.join("site.conf");
        fs::write(
            &site_path,
            format!("server {{ listen {listener}; location / {{ return 200 ok; }} }}"),
        )
        .expect("nginx site");
        let child = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .env("OXIROUTE_MANAGEMENT_TOKEN_FILE", &token_path)
            .env("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME", "1")
            .arg("serve")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start oxiroute");
        let server = Self {
            child,
            endpoint: format!("http://{management}"),
            token_path,
            site_path,
            _directory: directory,
        };
        server.wait_for_startup();
        server
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = cli();
        command
            .args([
                "--endpoint",
                &self.endpoint,
                "--token-file",
                self.token_path.to_str().expect("token path"),
                "--output",
                "json",
            ])
            .args(args)
            .output()
            .expect("oxiroute CLI")
    }

    fn generation_status(&self) -> Value {
        let output = self.run(&["generation", "status"]);
        assert!(output.status.success(), "{}", output_text(&output));
        serde_json::from_slice(&output.stdout).expect("generation status")
    }

    fn config(&self) -> Value {
        let output = self.run(&["config", "get"]);
        assert!(output.status.success(), "{}", output_text(&output));
        serde_json::from_slice(&output.stdout).expect("config response")
    }

    fn wait_for_startup(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let output = self.run(&["generation", "status"]);
            if output.status.success() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "oxiroute did not start: {}",
                output_text(&output)
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_active_change(&self, previous: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.generation_status();
            if status["generation"]["activeRevision"].as_str() != Some(previous) {
                return status;
            }
            assert!(Instant::now() < deadline, "generation did not activate");
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for NativeReloadServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reserve_tcp_address() -> SocketAddr {
    ports_support::unused_address()
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
