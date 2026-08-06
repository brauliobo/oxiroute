#![cfg(unix)]

use std::{collections::HashSet, fs, net::IpAddr, path::Path};

use oxiroute_import::{
    ImportReportEnvelope,
    apache::{E_REWRITE_UNSUPPORTED, import_root as import_apache},
    haproxy::{PreprocessingEnvironment, import_roots, import_roots_with_environment},
    nginx::{NginxImportOptions, import_root as import_nginx, import_root_with_options},
    squid::import as import_squid,
};
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn report_json_is_deterministic_and_identifies_each_source_product() {
    let directory = tempdir().expect("import report directory");
    let nginx_path = directory.path().join("nginx.conf");
    fs::write(
        &nginx_path,
        b"events {} http { access_log off; server { listen 127.0.0.1:18080 default_server; location / { return 200 ok; } } }",
    )
    .expect("nginx source");
    let apache_path = directory.path().join("httpd.conf");
    fs::write(
        &apache_path,
        b"Listen 127.0.0.1:18081\n<VirtualHost 127.0.0.1:18081>\n  ServerName app.example\n  ProxyPass / http://127.0.0.1:8080/\n</VirtualHost>\n",
    )
    .expect("Apache source");
    let haproxy_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/haproxy/minimal-representable.cfg");
    let squid_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/squid/hostrouter-sanitized.conf");

    let reports = [
        (
            "nginx",
            ImportReportEnvelope::from_nginx(&import_nginx(&nginx_path, directory.path())),
        ),
        (
            "haproxy",
            ImportReportEnvelope::from_haproxy(
                &import_roots(&[haproxy_path.clone()]),
                &[haproxy_path.clone()],
            ),
        ),
        (
            "squid",
            ImportReportEnvelope::from_squid(&import_squid(&squid_path)),
        ),
        (
            "apache",
            ImportReportEnvelope::from_apache(&import_apache(&apache_path)),
        ),
    ];

    for (product, report) in reports {
        let first = report.to_json().expect("report JSON");
        let second = report.to_json().expect("report JSON repeat");
        assert_eq!(first, second, "{product} report must be deterministic");
        let value: Value = serde_json::from_str(&first).expect("report object");
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["source"]["product"], product);
        assert!(value["source"]["version"].is_null());
        assert!(value["source"]["versionSource"].is_null());
        assert_eq!(
            value["source"]["capabilityProfile"]["version"],
            if product == "squid" { 2 } else { 1 }
        );
        assert!(
            value["sourceGraph"]["sources"]
                .as_array()
                .is_some_and(|sources| !sources.is_empty())
        );
        assert!(
            value["sourceGraph"]["sources"]
                .as_array()
                .unwrap()
                .iter()
                .all(|source| source["fingerprintSha256"]
                    .as_str()
                    .is_some_and(|fingerprint| fingerprint.len() == 64))
        );
        assert!(value["candidate"]["finalized"].is_boolean());
        if product == "squid" {
            assert_eq!(value["capabilities"]["targetVersion"], "6f4c814");
            assert_eq!(
                value["capabilities"]["profile"]["id"],
                "squid-forward-http1"
            );
            assert_eq!(value["capabilities"]["parity"], "partial");
            assert_eq!(value["capabilities"]["completeParity"], false);
        } else {
            assert!(value.get("capabilities").is_none());
        }
    }
}

#[test]
fn haproxy_report_identifies_strict_capability_and_retains_ordinary_source_provenance() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/haproxy/acl-conjunction.cfg");
    let report = ImportReportEnvelope::from_haproxy(
        &import_roots(std::slice::from_ref(&path)),
        std::slice::from_ref(&path),
    );
    let value: Value = serde_json::from_str(&report.to_json().expect("HAProxy report JSON"))
        .expect("HAProxy report object");

    assert_eq!(value["source"]["product"], "haproxy");
    assert!(value["source"]["version"].is_null());
    assert!(value["source"]["versionSource"].is_null());
    assert_eq!(value["source"]["capabilityProfile"]["id"], "haproxy-strict");
    assert_eq!(value["source"]["capabilityProfile"]["version"], 1);
    assert_eq!(value["sourceGraph"]["dependenciesComplete"], false);
    assert_eq!(value["sourceGraph"]["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["sourceMetadata"]["originalSourceIds"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(value["candidate"]["finalized"], true);
    assert!(
        value["candidate"]["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "/http_services/0/routes/0"
                && entry["origins"]
                    .as_array()
                    .is_some_and(|origins| origins.len() >= 3))
    );
}

#[test]
fn squid_report_keeps_open_capability_entries_out_of_complete_parity_claims() {
    let squid_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/squid/hostrouter-sanitized.conf");
    let report = ImportReportEnvelope::from_squid(&import_squid(&squid_path));
    let value: Value =
        serde_json::from_str(&report.to_json().expect("Squid capability report JSON"))
            .expect("Squid capability report object");
    let capabilities = &value["capabilities"];
    let families = capabilities["families"]
        .as_array()
        .expect("capability families");
    assert!(families.iter().any(|family| family["status"] == "partial"));
    assert!(
        families
            .iter()
            .any(|family| family["status"] == "unsupported")
    );
    assert_eq!(capabilities["completeParity"], false);
    assert_eq!(capabilities["registryVersion"], 2);
    assert_eq!(capabilities["profile"]["version"], 2);
    assert_ne!(capabilities["parity"], "complete");
    assert!(
        capabilities["directives"]
            .as_array()
            .is_some_and(|directives| {
                directives.iter().any(|directive| {
                    directive["key"] == "cache_peer" && directive["status"] == "unsupported"
                }) && directives.iter().any(|directive| {
                    directive["id"] == "directive.squid.cache-peer.static-parent"
                        && directive["status"] == "compatible"
                }) && directives.iter().any(|directive| {
                    directive["key"] == "always_direct" && directive["status"] == "compatible"
                })
            })
    );
}

#[test]
fn squid_report_serializes_source_resolvable_canonical_provenance() {
    let directory = tempdir().expect("Squid report directory");
    let included = directory.path().join("forward.conf");
    fs::write(
        &included,
        b"http_port 3128\n\
          access_log none\n\
          forwarded_for delete\n\
          via off\n\
          acl ssl_ports port 443\n\
          http_access deny CONNECT !ssl_ports\n\
          http_access allow all\n\
          cache_peer peer.example.test parent 3128 0\n\
          never_direct allow all\n",
    )
    .expect("Squid included source");
    let root = directory.path().join("squid.conf");
    fs::write(&root, b"include forward.conf\n").expect("Squid root source");

    let report = ImportReportEnvelope::from_squid(&import_squid(&root));
    let value: Value = serde_json::from_str(&report.to_json().expect("Squid report JSON"))
        .expect("Squid report object");
    let sources = value["sourceGraph"]["sources"]
        .as_array()
        .expect("serialized Squid sources");
    let provenance = value["candidate"]["provenance"]
        .as_array()
        .expect("serialized Squid provenance");
    assert!(!provenance.is_empty());
    assert!(
        provenance
            .iter()
            .any(|entry| { entry["path"] == "/forward_proxy_services/0/peer_policy/peers/0/host" })
    );
    assert!(
        provenance.iter().any(|entry| {
            entry["path"] == "/forward_proxy_services/0/peer_policy/direct_fallback"
        })
    );

    let mut paths = HashSet::new();
    for entry in provenance {
        let path = entry["path"].as_str().expect("provenance path");
        assert!(paths.insert(path), "duplicate serialized path {path}");
        let origins = entry["origins"].as_array().expect("provenance origins");
        assert!(!origins.is_empty(), "{path} has no serialized origins");
        for origin in origins {
            let source_id = origin["sourceId"].as_u64().expect("origin source ID");
            let source = sources
                .iter()
                .find(|source| source["id"] == source_id)
                .expect("origin source reference");
            assert!(source["path"].as_str().is_some());
            assert!(origin["range"]["start"].as_u64().is_some());
            assert!(origin["range"]["end"].as_u64().is_some());
            if path == "/listeners/0" {
                assert_eq!(
                    source["path"],
                    fs::canonicalize(&included).unwrap().to_str().unwrap()
                );
                assert_eq!(origin["includeStack"].as_array().unwrap().len(), 1);
            }
        }
    }
}

#[test]
fn report_retains_source_edges_environment_metadata_and_absent_maps() {
    let directory = tempdir().expect("import report directory");
    let root = directory.path().join("httpd.conf");
    let included = directory.path().join("site.conf");
    fs::write(&root, b"Include site.conf\n").expect("Apache root");
    fs::write(
        &included,
        b"Listen 127.0.0.1:18082\n<VirtualHost 127.0.0.1:18082>\n  ServerName app.example\n  ProxyPass / http://127.0.0.1:8080/\n</VirtualHost>\n",
    )
    .expect("Apache include");
    let apache = ImportReportEnvelope::from_apache(&import_apache(&root));
    let apache_json: Value = serde_json::from_str(&apache.to_json().expect("Apache JSON"))
        .expect("Apache report object");
    assert_eq!(
        apache_json["sourceGraph"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        apache_json["sourceGraph"]["dependencies"][0]["kind"],
        "include"
    );
    let dependency = &apache_json["sourceGraph"]["dependencies"][0];
    let target_id = dependency["targetSourceId"]
        .as_u64()
        .expect("Apache include target ID");
    let target = apache_json["sourceGraph"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["id"].as_u64() == Some(target_id))
        .expect("Apache include target source");
    assert_eq!(dependency["fingerprintSha256"], target["fingerprintSha256"]);
    assert_eq!(
        apache_json["sourceMetadata"]["sourceMaps"],
        Value::Array(Vec::new())
    );

    let haproxy_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/haproxy/minimal-representable.cfg");
    let haproxy = ImportReportEnvelope::from_haproxy(
        &import_roots_with_environment(
            &[haproxy_path.clone()],
            PreprocessingEnvironment {
                node_ip: "192.0.2.10".parse::<IpAddr>().expect("node IP"),
                gpu1_defined: false,
            },
        ),
        &[haproxy_path],
    );
    let haproxy_json: Value = serde_json::from_str(&haproxy.to_json().expect("HAProxy JSON"))
        .expect("HAProxy report object");
    assert_eq!(
        haproxy_json["sourceMetadata"]["environmentFingerprintSha256"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert_eq!(haproxy_json["sourceGraph"]["dependenciesComplete"], false);
    assert_eq!(
        haproxy_json["sourceGraph"]["dependencies"],
        Value::Array(Vec::new())
    );
    assert!(
        haproxy_json["sourceMetadata"]["sourceMaps"]
            .as_array()
            .is_some_and(|maps| !maps.is_empty())
    );
}

#[test]
fn report_exposes_blockers_requirements_and_satisfied_overlays() {
    let directory = tempdir().expect("import report directory");
    let blocked_path = directory.path().join("blocked-httpd.conf");
    fs::write(
        &blocked_path,
        b"Listen 127.0.0.1:18083\n<VirtualHost 127.0.0.1:18083>\n  ServerName blocked.example\n  RewriteEngine On\n  ProxyPass / http://127.0.0.1:8080/\n</VirtualHost>\n",
    )
    .expect("blocked Apache source");
    let blocked = import_apache(&blocked_path);
    assert!(blocked.has_errors());
    assert!(
        blocked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == E_REWRITE_UNSUPPORTED)
    );
    let blocked_json: Value = serde_json::from_str(
        &ImportReportEnvelope::from_apache(&blocked)
            .to_json()
            .expect("blocked report JSON"),
    )
    .expect("blocked report object");
    assert_eq!(blocked_json["candidate"]["finalized"], false);
    assert!(blocked_json["candidate"]["config"].is_null());
    assert!(blocked_json["blockers"].as_array().is_some_and(|blockers| {
        blockers
            .iter()
            .any(|blocker| blocker["code"] == E_REWRITE_UNSUPPORTED.as_str())
    }));
    assert!(blocked_json["blockers"].as_array().is_some_and(|blockers| {
        blockers.iter().any(|blocker| {
            blocker["kind"] == "virtual_host" && blocker["scope"] == "127.0.0.1:18083"
        })
    }));

    let nginx_path = directory.path().join("nginx.conf");
    fs::write(
        &nginx_path,
        b"user www-data; events { worker_connections 1024; } http { access_log off; server { listen 127.0.0.1:18084 default_server; location / { return 200 ok; } } }",
    )
    .expect("nginx requirements source");
    let nginx = import_root_with_options(
        &nginx_path,
        directory.path(),
        &NginxImportOptions::default(),
    );
    let nginx_json: Value = serde_json::from_str(
        &ImportReportEnvelope::from_nginx(&nginx)
            .to_json()
            .expect("nginx report JSON"),
    )
    .expect("nginx report object");
    assert!(
        nginx_json["requirements"]["deployment"]
            .as_array()
            .is_some_and(|requirements| {
                requirements
                    .iter()
                    .any(|requirement| requirement["kind"] == "process_user")
            })
    );

    let tls_path = directory.path().join("tls-httpd.conf");
    fs::write(
        &tls_path,
        b"Listen 127.0.0.1:18085\n<VirtualHost 127.0.0.1:18085>\n  ServerName secure.example\n  SSLEngine On\n  SSLCertificateFile /etc/ssl/certs/secure.pem\n  SSLCertificateKeyFile /etc/ssl/private/secure.key\n  ProxyPass / https://origin.example/\n</VirtualHost>\n",
    )
    .expect("Apache TLS source");
    let tls_json: Value = serde_json::from_str(
        &ImportReportEnvelope::from_apache(&import_apache(&tls_path))
            .to_json()
            .expect("TLS report JSON"),
    )
    .expect("TLS report object");
    assert!(tls_json["overlays"].as_array().is_some_and(|overlays| {
        overlays.iter().any(|overlay| {
            overlay["kind"] == "certificate_material" && overlay["satisfied"] == true
        })
    }));
}

#[test]
fn apache_report_distinguishes_missing_optional_includes_and_retains_include_stacks() {
    let directory = tempdir().expect("Apache report directory");
    let root = directory.path().join("httpd.conf");
    let included = directory.path().join("site.conf");
    fs::write(
        &root,
        b"IncludeOptional conf.d/missing.conf\nInclude site.conf\n",
    )
    .expect("Apache root");
    fs::write(
        &included,
        b"Listen 127.0.0.1:18086\n<VirtualHost 127.0.0.1:18086>\n  ServerName app.example\n  ProxyPass / http://127.0.0.1:8080/\n</VirtualHost>\n",
    )
    .expect("Apache included source");

    let report = ImportReportEnvelope::from_apache(&import_apache(&root));
    let value: Value = serde_json::from_str(&report.to_json().expect("Apache report JSON"))
        .expect("Apache report object");
    assert!(
        value["sourceGraph"]["dependencies"]
            .as_array()
            .expect("Apache dependencies")
            .iter()
            .any(|dependency| dependency["status"] == "optional_missing")
    );
    assert!(
        value["candidate"]["provenance"]
            .as_array()
            .expect("Apache provenance")
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
