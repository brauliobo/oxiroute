//! `oxiroute import` and the previews and reports it prints, in every source format.
mod cli_support;
use std::{fs, path::Path};

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{ConfigFormat, resolve_source_with_format};
use serde_json::Value;
use tempfile::TempDir;

use cli_support::{cli, output_text};

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
        .join("../oxiroute-import/tests/fixtures/squid/host-a-sanitized.conf");
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
        .join("../oxiroute-import/tests/fixtures/squid/host-a-sanitized.conf");
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
        .as_draft()
        .http_services
        .len(),
        1
    );
}

fn import_preview(args: &[&str], format_name: Option<&str>, format: ConfigFormat) -> ConfigDraft {
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
        .to_draft()
}
