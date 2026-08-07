use std::{
    fs,
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt as _,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use oxiroute_config::Config;
use oxiroute_config_source::{
    ConfigFormat, decode_value, render_config, resolve_source_with_format,
};
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
        "{\"ready\":true,\"schemaVersion\":1}\n"
    );
    assert!(output.stderr.is_empty());
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
        "{\"ready\":true,\"schemaVersion\":1}\n"
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve endpoint");
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
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
    let squid_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    let apache_path = directory.path().join("httpd.conf");
    fs::write(
        &apache_path,
        b"Listen 127.0.0.1:18081\n<VirtualHost 127.0.0.1:18081>\n  ServerName proxy.example\n  ProxyPass / http://127.0.0.1:8080/\n</VirtualHost>\n",
    )
    .expect("Apache source");
    let nginx_args = ["import", "nginx", nginx_path.to_str().unwrap()];
    let haproxy_args = ["import", "haproxy", haproxy_path.to_str().unwrap()];
    let squid_args = ["import", "squid", squid_path.to_str().unwrap()];
    let apache_args = ["import", "apache", apache_path.to_str().unwrap()];

    let default_nginx = import_preview(&nginx_args, None, ConfigFormat::Kdl);
    let default_haproxy = import_preview(&haproxy_args, None, ConfigFormat::Kdl);
    let default_squid = import_preview(&squid_args, None, ConfigFormat::Kdl);
    let default_apache = import_preview(&apache_args, None, ConfigFormat::Kdl);
    assert_eq!(default_nginx.listeners.len(), 1);
    assert_eq!(default_haproxy.listeners.len(), 1);
    assert_eq!(default_squid.listeners.len(), 1);
    assert_eq!(default_apache.listeners.len(), 1);

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
        assert_eq!(
            import_preview(&squid_args, Some(name), format),
            default_squid,
            "Squid {name} preview"
        );
        assert_eq!(
            import_preview(&apache_args, Some(name), format),
            default_apache,
            "Apache {name} preview"
        );
    }
}

#[test]
fn apache_cli_report_exposes_deterministic_include_provenance() {
    let directory = TempDir::new().expect("directory");
    let root = directory.path().join("httpd.conf");
    let included = directory.path().join("site.conf");
    fs::write(&root, b"Include site.conf\n").expect("Apache root");
    fs::write(
        &included,
        b"Listen 127.0.0.1:18088\n<VirtualHost 127.0.0.1:18088>\n  ServerName app.example.test\n  ProxyPass / http://127.0.0.1:9000/\n</VirtualHost>\n",
    )
    .expect("Apache included source");

    let output = cli()
        .args([
            "import",
            "apache",
            root.to_str().unwrap(),
            "--output",
            "report",
        ])
        .output()
        .expect("Apache CLI report");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("Apache report JSON");
    assert_eq!(report["candidate"]["finalized"], true);
    assert_eq!(
        report["sourceGraph"]["sources"].as_array().unwrap().len(),
        2
    );
    assert!(
        report["candidate"]["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["origins"].as_array().is_some_and(|origins| {
                    origins.iter().any(|origin| {
                        origin["includeStack"]
                            .as_array()
                            .is_some_and(|stack| !stack.is_empty())
                    })
                })
            })
    );
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

#[test]
fn import_report_is_deterministic_json_and_preview_remains_canonical_output() {
    let haproxy_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/minimal-representable.cfg");
    let path = haproxy_path.to_str().unwrap();
    let report = cli()
        .args(["import", "haproxy", path])
        .output()
        .expect("import report");
    let repeated = cli()
        .args(["import", "haproxy", path])
        .output()
        .expect("repeated import report");
    assert!(report.status.success(), "{}", output_text(&report));
    assert!(repeated.status.success(), "{}", output_text(&repeated));
    assert_eq!(report.stdout, repeated.stdout);
    let report_json: Value = serde_json::from_slice(&report.stdout).expect("report JSON");
    assert_eq!(report_json["schemaVersion"], 1);
    assert_eq!(report_json["source"]["product"], "haproxy");
    assert!(
        report_json["sourceGraph"]["sources"]
            .as_array()
            .is_some_and(|sources| !sources.is_empty())
    );
    assert!(
        report_json["sourceGraph"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["fingerprintSha256"].as_str().is_some())
    );

    let preview = cli()
        .args(["import", "haproxy", path, "--output", "preview"])
        .output()
        .expect("import preview");
    assert!(preview.status.success(), "{}", output_text(&preview));
    assert!(preview.stdout.ends_with(b"version 1\n"));
    assert_ne!(report.stdout, preview.stdout);
}

#[test]
fn haproxy_acl_conjunction_report_retains_capability_source_table_provenance_and_blockers() {
    let conjunction_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/acl-conjunction.cfg");
    let report = cli()
        .args(["import", "haproxy", conjunction_path.to_str().unwrap()])
        .output()
        .expect("HAProxy conjunction report");
    assert!(report.status.success(), "{}", output_text(&report));
    let report_json: Value = serde_json::from_slice(&report.stdout).expect("report JSON");
    assert_eq!(report_json["source"]["product"], "haproxy");
    assert!(report_json["source"]["version"].is_null());
    assert!(report_json["source"]["versionSource"].is_null());
    assert_eq!(
        report_json["source"]["capabilityProfile"]["id"],
        "haproxy-strict"
    );
    assert_eq!(report_json["source"]["capabilityProfile"]["version"], 1);
    assert_eq!(
        report_json["sourceGraph"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        report_json["sourceMetadata"]["originalSourceIds"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(report_json["candidate"]["finalized"], true);
    assert!(
        report_json["candidate"]["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "/http_services/0/routes/0"
                && entry["origins"]
                    .as_array()
                    .is_some_and(|origins| origins.len() >= 3))
    );

    let directory = TempDir::new().expect("blocked HAProxy report directory");
    let blocked_path = directory.path().join("blocked.cfg");
    fs::write(
        &blocked_path,
        b"frontend public\n  mode http\n  bind 127.0.0.1:18080\n  use_backend app if { path /api }\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
    )
    .expect("blocked HAProxy source");
    let blocked = cli()
        .args(["import", "haproxy", blocked_path.to_str().unwrap()])
        .output()
        .expect("blocked HAProxy report");
    assert!(blocked.status.success(), "{}", output_text(&blocked));
    let blocked_json: Value = serde_json::from_slice(&blocked.stdout).expect("blocked report JSON");
    assert_eq!(blocked_json["candidate"]["finalized"], false);
    assert!(
        blocked_json["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| {
                blocker["code"] == "E_UNSUPPORTED_FORM"
                    && blocker["origins"]
                        .as_array()
                        .is_some_and(|origins| !origins.is_empty())
            })
    );
    assert!(
        blocked_json["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "E_UNSUPPORTED_FORM"
                && diagnostic["stage"] == "resolve")
    );
}

#[test]
fn squid_import_report_publishes_the_target_capability_registry() {
    let squid_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    let report = cli()
        .args(["import", "squid", squid_path.to_str().unwrap()])
        .output()
        .expect("Squid import report");
    assert!(report.status.success(), "{}", output_text(&report));
    let report_json: Value = serde_json::from_slice(&report.stdout).expect("Squid report JSON");
    assert_eq!(report_json["source"]["product"], "squid");
    assert_eq!(report_json["capabilities"]["targetVersion"], "6f4c814");
    assert_eq!(report_json["capabilities"]["registryVersion"], 3);
    assert_eq!(report_json["capabilities"]["profile"]["version"], 3);
    assert_eq!(report_json["capabilities"]["parity"], "partial");
    assert_eq!(report_json["capabilities"]["completeParity"], false);
    assert!(
        report_json["capabilities"]["families"]
            .as_array()
            .is_some_and(|families| families.iter().any(|family| {
                family["id"] == "family.squid.cache" && family["status"] == "unsupported"
            }))
    );
    assert!(
        report_json["capabilities"]["directives"]
            .as_array()
            .is_some_and(|directives| {
                directives.iter().any(|directive| {
                    directive["id"] == "directive.squid.cache-peer.static-parent"
                        && directive["status"] == "compatible"
                })
            })
    );
}

#[test]
fn varnish_import_report_and_preview_use_exact_invocation_arguments() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/varnish/exact.vcl");
    let invocation = [
        "varnishd",
        "-a",
        ":6081",
        "-s",
        "cache=malloc,256M",
        "-p",
        "default_ttl=120s",
        "-p",
        "default_grace=10s",
        "-p",
        "default_keep=300s",
        "-F",
    ];
    let mut report_command = cli();
    report_command.args(["import", "varnish", root.to_str().unwrap()]);
    for argument in invocation {
        report_command.args(["--arg", argument]);
    }
    let report = report_command.output().expect("Varnish report");
    assert!(report.status.success(), "{}", output_text(&report));
    let report_json: Value = serde_json::from_slice(&report.stdout).expect("Varnish report JSON");
    assert_eq!(report_json["source"]["product"], "varnish");
    assert_eq!(
        report_json["source"]["capabilityProfile"]["id"],
        "varnish-vcl-exact-cache"
    );
    assert_eq!(report_json["candidate"]["finalized"], true);

    let mut preview_command = cli();
    preview_command.args([
        "import",
        "varnish",
        root.to_str().unwrap(),
        "--output",
        "preview",
    ]);
    for argument in invocation {
        preview_command.args(["--arg", argument]);
    }
    let preview = preview_command.output().expect("Varnish preview");
    assert!(preview.status.success(), "{}", output_text(&preview));
    assert!(preview.stdout.ends_with(b"version 1\n"));
    assert_eq!(
        resolve_source_with_format(
            Path::new("varnish-preview"),
            &preview.stdout,
            ConfigFormat::Kdl
        )
        .expect("Varnish preview round trip")
        .config
        .http_services
        .len(),
        1
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
        let deadline = Instant::now() + Duration::from_secs(5);
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
        let deadline = Instant::now() + Duration::from_secs(5);
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve address");
    listener.local_addr().expect("reserved address")
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
