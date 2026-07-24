use std::{collections::BTreeSet, fs, path::Path};

use oxiroute_import::{
    DiagnosticStage,
    nginx::{ImportReport as NginxImportReport, OccurrenceDisposition, import_http_fragment},
};
use tempfile::TempDir;

use crate::{
    manifests::{DirectiveForm, DirectiveManifest, Disposition},
    report_invariants::{
        assert_diagnostic_message, assert_import_report_invariants,
        import_nginx_plaintext_supported_fixture,
    },
    support::{assert_set_equality, read_manifest, read_source, workspace_path},
};

#[test]
fn nginx_manifest_forms_execute_parser_semantic_and_lowering_decisions() {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let registration = import_nginx_registration_fixture();
    assert_import_report_invariants(&registration);
    let registered = registration
        .occurrence_ledger
        .iter()
        .map(|decision| String::from_utf8_lossy(&decision.name.value).into_owned())
        .collect::<BTreeSet<_>>();
    let manifested = manifest
        .entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect::<BTreeSet<_>>();
    assert_set_equality(
        "nginx parser/semantic registrations",
        &registered,
        &manifested,
    );

    for entry in &manifest.entries {
        let report = import_nginx_probe(entry);
        assert_nginx_probe(entry, &report, "primary form");
    }
}

#[test]
fn nginx_manifest_contexts_and_cross_directive_requirements_are_executable() {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    for entry in &manifest.entries {
        for context in &entry.contexts {
            let report = import_nginx_context_probe(entry, context);
            assert_nginx_probe(entry, &report, context);
        }
    }

    for (id, label, source) in [
        (
            "directive.nginx.listen.static",
            "IPv6 static listen",
            render_nginx_fixture(NginxFixtureSpec {
                listen: "[::1]:8443 ssl default_server",
                ..standard_nginx_fixture()
            }),
        ),
        (
            "directive.nginx.server-name.incompatible-wildcard",
            "trailing wildcard",
            nginx_with_nondefault_server("api.*"),
        ),
        (
            "directive.nginx.location.special",
            "regex location",
            render_nginx_fixture(NginxFixtureSpec {
                location: "location ~ ^/regex",
                ..standard_nginx_fixture()
            }),
        ),
        (
            "directive.nginx.location.special",
            "named location",
            render_nginx_fixture(NginxFixtureSpec {
                location: "location @fallback",
                ..standard_nginx_fixture()
            }),
        ),
        (
            "directive.nginx.location.special",
            "variable location",
            render_nginx_fixture(NginxFixtureSpec {
                location: "location /$tenant",
                ..standard_nginx_fixture()
            }),
        ),
    ] {
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("missing nginx legal-form manifest entry {id}"));
        let report = import_nginx_source(source);
        assert_nginx_probe(entry, &report, label);
    }

    let include = manifest
        .entries
        .iter()
        .find(|entry| entry.id == "directive.nginx.include.path")
        .expect("nginx include manifest entry");
    let include_directory = TempDir::new().expect("create nginx glob include probe directory");
    fs::write(
        include_directory.path().join("site-a.conf"),
        render_nginx_fixture(standard_nginx_fixture()),
    )
    .expect("write nginx glob include target");
    fs::write(
        include_directory.path().join("nginx.conf"),
        "include site-*.conf;\n",
    )
    .expect("write nginx glob include root");
    let glob_include = import_http_fragment(Path::new("nginx.conf"), include_directory.path());
    assert_nginx_probe(include, &glob_include, "glob include");

    let defaults = import_nginx_plaintext_supported_fixture();
    assert_diagnostic_message(&defaults.diagnostics, "proxy defaults");

    let unequal_timeouts = import_nginx_source(
        render_nginx_fixture(standard_nginx_fixture())
            .replace("proxy_send_timeout 15s", "proxy_send_timeout 16s"),
    );
    assert_diagnostic_message(
        &unequal_timeouts.diagnostics,
        "timeouts are not one uniform I/O timeout",
    );

    let bad_key = import_nginx_source(nginx_mismatched_key_probe());
    assert_diagnostic_message(&bad_key.diagnostics, "private key material");

    let directory = TempDir::new().expect("create nginx glob grammar probe directory");
    fs::write(
        directory.path().join("nginx.conf"),
        "include matches/[z-a].conf;\n",
    )
    .expect("write nginx glob grammar probe");
    fs::create_dir(directory.path().join("matches")).expect("create nginx glob probe directory");
    let invalid_glob = import_http_fragment(Path::new("nginx.conf"), directory.path());
    assert_diagnostic_message(&invalid_glob.diagnostics, "glob grammar");
}

#[test]
fn nginx_exposes_only_the_report_preserving_canonical_import_entry_point() {
    let module = read_source("crates/oxiroute-import/src/nginx/mod.rs");
    let lower = read_source("crates/oxiroute-import/src/nginx/lower.rs");

    assert!(!module.contains("lower_http"));
    assert!(!lower.contains("pub fn lower_http"));
    assert!(module.contains("import_http_fragment"));
    assert!(!module.contains("import_http,"));
}

fn assert_nginx_probe(entry: &DirectiveForm, report: &NginxImportReport, label: &str) {
    assert_import_report_invariants(report);
    let decisions = report
        .occurrence_ledger
        .iter()
        .filter(|decision| decision.name.value == entry.key.as_bytes())
        .collect::<Vec<_>>();
    assert!(
        !decisions.is_empty(),
        "{} was not parsed ({label})",
        entry.id
    );
    match entry.disposition {
        Disposition::Lowered => {
            assert!(
                report.config.is_some(),
                "{} did not finalize its exact lowering probe ({label}): {:?}",
                entry.id,
                report.diagnostics
            );
            assert!(
                decisions.iter().any(|decision| matches!(
                    decision.disposition,
                    OccurrenceDisposition::Resolved | OccurrenceDisposition::Structural
                )),
                "{} has no resolved lowering decision ({label})",
                entry.id
            );
        }
        Disposition::Classified => assert!(
            decisions.iter().any(|decision| matches!(
                decision.disposition,
                OccurrenceDisposition::Resolved | OccurrenceDisposition::Structural
            )),
            "{} was not explicitly classified ({label})",
            entry.id
        ),
        Disposition::Blocked => assert!(
            decisions
                .iter()
                .any(|decision| matches!(decision.disposition, OccurrenceDisposition::Blocking(_)))
                || report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.stage() == DiagnosticStage::Lower),
            "{} has no semantic or lowering blocker ({label})",
            entry.id
        ),
        Disposition::Externalized => panic!("nginx form cannot be externalized: {}", entry.id),
    }
}

fn import_nginx_registration_fixture() -> NginxImportReport {
    let directory = TempDir::new().expect("create nginx registration fixture directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"include registered.conf;\n",
    )
    .expect("write nginx registration root");
    fs::write(
        directory.path().join("registered.conf"),
        render_nginx_fixture(NginxFixtureSpec {
            extra_http: "http2 on;",
            extra_server: "location = /fixed { return 204; }\n    location = /redirect { return 308 https://example.test/new; }\n    location /static { root /srv/static; index index.html; }\n    location /protected { auth_basic synthetic; auth_basic_user_file /tmp/synthetic-users; return 403; }",
            ..standard_nginx_fixture()
        }),
    )
    .expect("write nginx registration include");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

fn import_nginx_probe(entry: &DirectiveForm) -> NginxImportReport {
    let directory = TempDir::new().expect("create nginx directive probe directory");
    let source = nginx_probe_source(entry.id.as_str(), directory.path());
    fs::write(directory.path().join("nginx.conf"), source).expect("write nginx directive probe");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

fn import_nginx_context_probe(entry: &DirectiveForm, context: &str) -> NginxImportReport {
    let directory = TempDir::new().expect("create nginx context probe directory");
    let source = nginx_context_probe_source(entry, context, directory.path());
    fs::write(directory.path().join("nginx.conf"), source).expect("write nginx context probe");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

fn import_nginx_source(source: String) -> NginxImportReport {
    let directory = TempDir::new().expect("create nginx source probe directory");
    fs::write(directory.path().join("nginx.conf"), source).expect("write nginx source probe");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

fn nginx_context_probe_source(entry: &DirectiveForm, context: &str, directory: &Path) -> String {
    let source = nginx_probe_source(entry.id.as_str(), directory);
    if entry.key == "location" && context == "location" {
        return nginx_nested_location_probe(entry.id.as_str());
    }
    if entry.contexts.len() == 1 || entry.contexts == ["any"] {
        return source;
    }
    move_nginx_directive(source, &entry.key, context)
}

fn move_nginx_directive(mut source: String, key: &str, context: &str) -> String {
    let prefix = format!("{key} ");
    let line = source
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .unwrap_or_else(|| panic!("nginx context probe has no `{key}` directive"));
    let directive = line.trim().to_owned();
    let physical_line = format!("{line}\n");
    source = source.replacen(&physical_line, "", 1);
    match context {
        "http" => source.replacen("http {\n", &format!("http {{\n  {directive}\n"), 1),
        "http_server" => source.replacen(
            "  server {\n",
            &format!("  server {{\n    {directive}\n"),
            1,
        ),
        "location" => source.replacen(
            "    location / { proxy_pass http://app; }\n",
            &format!("    location / {{ {directive} proxy_pass http://app; }}\n"),
            1,
        ),
        value => panic!("unsupported nginx directive context `{value}` for `{key}`"),
    }
}

fn nginx_nested_location_probe(id: &str) -> String {
    let nested = match id {
        "directive.nginx.location.prefix" => "location /outer/inner { proxy_pass http://app; }",
        "directive.nginx.location.exact" => "location = /outer/health { proxy_pass http://app; }",
        "directive.nginx.location.special" => {
            "location ^~ /outer/assets { proxy_pass http://app; }"
        }
        value => panic!("nginx location form has no nested probe: {value}"),
    };
    render_nginx_fixture(standard_nginx_fixture()).replace(
        "location / { proxy_pass http://app; }",
        &format!("location /outer {{ {nested} }}"),
    )
}

fn nginx_mismatched_key_probe() -> String {
    let original_certificate = fs::canonicalize(workspace_path(
        "crates/oxiroute-server/tests/fixtures/proxy-a.pem",
    ))
    .expect("canonical original certificate fixture");
    let original_key = fs::canonicalize(workspace_path(
        "crates/oxiroute-server/tests/fixtures/proxy-a-key.pem",
    ))
    .expect("canonical original key fixture");
    let certificate = fs::canonicalize(workspace_path(
        "crates/oxiroute-import/tests/fixtures/nginx/proxy.pem",
    ))
    .expect("canonical nginx certificate fixture");
    let key = fs::canonicalize(workspace_path(
        "crates/oxiroute-import/tests/fixtures/nginx/proxy-mismatched-key.pem",
    ))
    .expect("canonical mismatched nginx key fixture");
    render_nginx_fixture(standard_nginx_fixture())
        .replace(
            original_certificate.to_string_lossy().as_ref(),
            certificate.to_string_lossy().as_ref(),
        )
        .replace(
            original_key.to_string_lossy().as_ref(),
            key.to_string_lossy().as_ref(),
        )
}

fn nginx_probe_source(id: &str, directory: &Path) -> String {
    match id {
        "directive.nginx.include.path" => nginx_include_probe(directory),
        id if id.starts_with("directive.nginx.listen.") => nginx_listen_probe(id),
        id if id.starts_with("directive.nginx.server-name.") => nginx_server_name_probe(id),
        id if id.starts_with("directive.nginx.location.") => nginx_location_probe(id),
        id if id.starts_with("directive.nginx.proxy-pass.") => nginx_proxy_pass_probe(id),
        "directive.nginx.proxy-pass-header.date" => render_nginx_fixture(standard_nginx_fixture())
            .replace("proxy_pass_header Server;", "proxy_pass_header Date;"),
        id if id.starts_with("directive.nginx.proxy-http-version.") => {
            nginx_proxy_http_version_probe(id)
        }
        id if id.starts_with("directive.nginx.return.") => nginx_return_probe(id),
        id if id.starts_with("directive.nginx.auth-basic.") => nginx_auth_basic_probe(id),
        "directive.nginx.auth-basic-user-file" => nginx_auth_basic_probe(id),
        "directive.nginx.root.static" | "directive.nginx.index.static" => nginx_static_probe(),
        "directive.nginx.http2" => render_nginx_fixture(NginxFixtureSpec {
            listen: "127.0.0.1:8443 ssl default_server",
            extra_http: "http2 on;",
            ..standard_nginx_fixture()
        }),
        "directive.nginx.http.block"
        | "directive.nginx.upstream.named-block"
        | "directive.nginx.upstream-server.static"
        | "directive.nginx.http-server.block"
        | "directive.nginx.client-max-body-size"
        | "directive.nginx.proxy-connect-timeout"
        | "directive.nginx.proxy-read-timeout"
        | "directive.nginx.proxy-send-timeout"
        | "directive.nginx.proxy-buffering.off"
        | "directive.nginx.proxy-request-buffering.off"
        | "directive.nginx.proxy-next-upstream.safe"
        | "directive.nginx.proxy-next-upstream-tries.bounded"
        | "directive.nginx.proxy-set-header.exact"
        | "directive.nginx.proxy-hide-header.exact"
        | "directive.nginx.proxy-pass-header.classified"
        | "directive.nginx.proxy-ignore-headers.controls"
        | "directive.nginx.proxy-cookie-path.literal"
        | "directive.nginx.ssl-certificate"
        | "directive.nginx.ssl-certificate-key"
        | "directive.nginx.ssl-protocols" => render_nginx_fixture(standard_nginx_fixture()),
        id => panic!("nginx manifest form has no executable probe: {id}"),
    }
}

fn nginx_include_probe(directory: &Path) -> String {
    fs::write(
        directory.join("site.conf"),
        render_nginx_fixture(standard_nginx_fixture()),
    )
    .expect("write nginx include probe");
    "include site.conf;\n".to_owned()
}

fn nginx_listen_probe(id: &str) -> String {
    let listen = match id {
        "directive.nginx.listen.static" => standard_nginx_fixture().listen,
        "directive.nginx.listen.variable" => "127.0.0.1:$port",
        "directive.nginx.listen.unsupported-option" => "127.0.0.1:8443 reuseport",
        id => panic!("nginx listen form has no executable probe: {id}"),
    };
    render_nginx_fixture(NginxFixtureSpec {
        listen,
        ..standard_nginx_fixture()
    })
}

fn nginx_server_name_probe(id: &str) -> String {
    match id {
        "directive.nginx.server-name.incompatible-wildcard" => {
            nginx_with_nondefault_server(".example.test")
        }
        "directive.nginx.server-name.leading-wildcard" => {
            nginx_with_nondefault_server("*.example.test")
        }
        "directive.nginx.server-name.canonical" => {
            nginx_with_nondefault_server("exact.example.test")
        }
        "directive.nginx.server-name.regex" => render_nginx_fixture(NginxFixtureSpec {
            server_name: "~^app\\.example$",
            ..standard_nginx_fixture()
        }),
        "directive.nginx.server-name.variable" => render_nginx_fixture(NginxFixtureSpec {
            server_name: "$host",
            ..standard_nginx_fixture()
        }),
        id => panic!("nginx server_name form has no executable probe: {id}"),
    }
}

fn nginx_location_probe(id: &str) -> String {
    match id {
        "directive.nginx.location.prefix" => render_nginx_fixture(standard_nginx_fixture()),
        "directive.nginx.location.exact" => render_nginx_fixture(NginxFixtureSpec {
            location: "location = /health",
            extra_server: "location / { proxy_pass http://app; }",
            ..standard_nginx_fixture()
        }),
        "directive.nginx.location.special" => render_nginx_fixture(NginxFixtureSpec {
            location: "location ^~ /api",
            ..standard_nginx_fixture()
        }),
        id => panic!("nginx location form has no executable probe: {id}"),
    }
}

fn nginx_proxy_pass_probe(id: &str) -> String {
    let proxy_pass = match id {
        "directive.nginx.proxy-pass.static-http" => standard_nginx_fixture().proxy_pass,
        "directive.nginx.proxy-pass.uri-replacement" => "http://app/v1/",
        "directive.nginx.proxy-pass.variable" => "http://$backend",
        "directive.nginx.proxy-pass.https-defaults" => "https://app",
        "directive.nginx.proxy-pass.unresolved" => "http://missing_pool",
        id => panic!("nginx proxy_pass form has no executable probe: {id}"),
    };
    render_nginx_fixture(NginxFixtureSpec {
        proxy_pass,
        ..standard_nginx_fixture()
    })
}

fn nginx_proxy_http_version_probe(id: &str) -> String {
    let proxy_http_version = match id {
        "directive.nginx.proxy-http-version.11" => standard_nginx_fixture().proxy_http_version,
        "directive.nginx.proxy-http-version.other" => "1.0",
        id => panic!("nginx proxy_http_version form has no executable probe: {id}"),
    };
    render_nginx_fixture(NginxFixtureSpec {
        proxy_http_version,
        ..standard_nginx_fixture()
    })
}

fn nginx_return_probe(id: &str) -> String {
    let action = match id {
        "directive.nginx.return.fixed" => "return 204;",
        "directive.nginx.return.redirect" => "return 308 https://example.test/new;",
        id => panic!("nginx return form has no executable probe: {id}"),
    };
    render_nginx_fixture(standard_nginx_fixture()).replace("proxy_pass http://app;", action)
}

fn nginx_static_probe() -> String {
    render_nginx_fixture(standard_nginx_fixture()).replace(
        "proxy_pass http://app;",
        "root /srv/static;\n      index index.html home.html;",
    )
}

fn nginx_auth_basic_probe(id: &str) -> String {
    let policy = match id {
        "directive.nginx.auth-basic.off" => "auth_basic off;",
        "directive.nginx.auth-basic.enabled" => "auth_basic synthetic;",
        "directive.nginx.auth-basic-user-file" => {
            "auth_basic synthetic;\n      auth_basic_user_file /tmp/synthetic-users;"
        }
        id => panic!("nginx auth form has no executable probe: {id}"),
    };
    render_nginx_fixture(standard_nginx_fixture()).replace(
        "proxy_pass http://app;",
        &format!("{policy}\n      proxy_pass http://app;"),
    )
}

#[derive(Clone, Copy)]
struct NginxFixtureSpec<'a> {
    listen: &'a str,
    server_name: &'a str,
    location: &'a str,
    proxy_pass: &'a str,
    proxy_http_version: &'a str,
    extra_http: &'a str,
    extra_server: &'a str,
    upstream_server: &'a str,
}

const fn standard_nginx_fixture() -> NginxFixtureSpec<'static> {
    NginxFixtureSpec {
        listen: "127.0.0.1:8443 ssl http2 default_server",
        server_name: "proxy.example.test",
        location: "location /",
        proxy_pass: "http://app",
        proxy_http_version: "1.1",
        extra_http: "",
        extra_server: "",
        upstream_server: "127.0.0.1:3000",
    }
}

fn render_nginx_fixture(spec: NginxFixtureSpec<'_>) -> String {
    let certificate = fs::canonicalize(workspace_path(
        "crates/oxiroute-server/tests/fixtures/proxy-a.pem",
    ))
    .expect("canonical certificate fixture path");
    let private_key = fs::canonicalize(workspace_path(
        "crates/oxiroute-server/tests/fixtures/proxy-a-key.pem",
    ))
    .expect("canonical private-key fixture path");
    format!(
        "http {{\n  client_max_body_size 2m;\n  proxy_connect_timeout 15s;\n  proxy_read_timeout 15s;\n  proxy_send_timeout 15s;\n  proxy_http_version {};\n  proxy_buffering off;\n  proxy_request_buffering off;\n  proxy_next_upstream off;\n  proxy_next_upstream_tries 1;\n  proxy_set_header Host $http_host;\n  proxy_hide_header X-Powered-By;\n  proxy_pass_header Server;\n  proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;\n  proxy_cookie_path / /application;\n  auth_basic off;\n  {}\n  upstream app {{ server {}; }}\n  server {{\n    listen {};\n    server_name {};\n    ssl_certificate {};\n    ssl_certificate_key {};\n    ssl_protocols TLSv1.2 TLSv1.3;\n    {}\n    {} {{ proxy_pass {}; }}\n  }}\n}}\n",
        spec.proxy_http_version,
        spec.extra_http,
        spec.upstream_server,
        spec.listen,
        spec.server_name,
        certificate.display(),
        private_key.display(),
        spec.extra_server,
        spec.location,
        spec.proxy_pass
    )
}

fn nginx_with_nondefault_server(server_name: &str) -> String {
    let mut source = render_nginx_fixture(NginxFixtureSpec {
        listen: "127.0.0.1:18080 default_server",
        server_name: "default.example.test",
        ..standard_nginx_fixture()
    });
    let end = source.rfind("}\n").expect("nginx http closing brace");
    source.insert_str(
        end,
        &format!(
            "  server {{ listen 127.0.0.1:18080; server_name {server_name}; location / {{ proxy_pass http://app; }} }}\n"
        ),
    );
    source
}
