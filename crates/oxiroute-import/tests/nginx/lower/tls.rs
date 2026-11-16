use std::{fs, path::Path};

use oxiroute_config::{CertificateSource, KtlsMode, TlsPolicy, TlsSessionCache};
use oxiroute_import::nginx::import_http_fragment;

use super::support::{copy_secure_key, fixture, import_source};

#[test]
fn accepts_matching_secure_test_key_without_exposing_material() {
    let directory = fixture("nginx/representable.conf");
    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());

    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("private key material") })
    );
}

#[test]
fn merges_one_certificate_lineage_across_distinct_tls_binds() {
    let directory = tempfile::tempdir().expect("create TLS source directory");
    let certificate =
        fs::canonicalize("tests/fixtures/nginx/proxy.pem").expect("canonical certificate fixture");
    let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
    let source = format!(
        r"http {{
          access_log off;
          proxy_http_version 1.1;
          proxy_buffering off;
          proxy_request_buffering off;
          proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;
          upstream backend {{ server 127.0.0.1:8080; }}
          server {{
            listen 127.0.0.1:8443 ssl default_server;
            server_name first.example.test;
            ssl_certificate {};
            ssl_certificate_key {};
            ssl_protocols TLSv1.2 TLSv1.3;
            location / {{ proxy_pass http://backend; }}
          }}
          server {{
            listen 127.0.0.1:8444 ssl default_server;
            server_name second.example.test;
            ssl_certificate {};
            ssl_certificate_key {};
            ssl_protocols TLSv1.2 TLSv1.3;
            location / {{ proxy_pass http://backend; }}
          }}
        }}",
        certificate.display(),
        private_key.display(),
        certificate.display(),
        private_key.display(),
    );
    fs::write(directory.path().join("nginx.conf"), source).expect("write TLS source");

    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
    assert!(report.blocked_services.is_empty());
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("distinct TLS listeners");
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.certificates[0].dns_names.len(), 2);
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("private key material") })
    );
}

#[test]
fn lowers_certificate_paths_without_reading_operational_material() {
    let directory = tempfile::tempdir().expect("create TLS source directory");
    let source = fs::read_to_string("tests/fixtures/nginx/representable.conf")
        .expect("read representable TLS fixture")
        .replacen("http {", "http { access_log off;", 1)
        .replace("@CERTIFICATE@", "/definitely/missing/fullchain.pem")
        .replace("@PRIVATE_KEY@", "/definitely/missing/privkey.pem");
    fs::write(directory.path().join("nginx.conf"), source).expect("write TLS source");

    let report = import_http_fragment(Path::new("nginx.conf"), directory.path());
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("path-based TLS config");
    assert!(report.blocked_services.is_empty());
    assert_eq!(config.certificates[0].dns_names.len(), 2);
    assert_eq!(
        config.certificates[0].source,
        CertificateSource::Files {
            certificate_chain_path: "/definitely/missing/fullchain.pem".into(),
            private_key_path: "/definitely/missing/privkey.pem".into(),
        }
    );
    assert!(report.diagnostics.iter().all(|diagnostic| {
        !diagnostic.message().contains("certificate metadata")
            && !diagnostic.message().contains("private key material")
    }));
}

#[test]
fn lowers_inherited_nginx_tls_policy_exactly() {
    let report = import_source(
        r#"http {
          ssl_ciphers "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:DHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES256-GCM-SHA384";
          ssl_dhparam /etc/letsencrypt/ssl-dhparams.pem;
          ssl_session_cache shared:le_nginx_SSL:10m;
          ssl_session_timeout 1440m;
          ssl_session_tickets off;
          ssl_prefer_server_ciphers off;
          server {
            listen 127.0.0.1:8443 ssl default_server;
            server_name host-f.example;
            ssl_certificate /definitely/missing/fullchain.pem;
            ssl_certificate_key /definitely/missing/privkey.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            location / { return 204; }
          }
        }"#,
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("canonical nginx TLS policy");
    assert!(report.blocked_services.is_empty());
    assert_eq!(
        config.tls_profiles[0].policy,
        TlsPolicy {
            cipher_list: Some("ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:DHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES256-GCM-SHA384".into()),
            dh_parameters_path: Some("/etc/letsencrypt/ssl-dhparams.pem".into()),
            client_auth: oxiroute_config::TlsClientAuthPolicy::default(),
            session_cache: Some(TlsSessionCache {
                name: "le_nginx_SSL".into(),
                size_bytes: 10 * 1024 * 1024,
            }),
            session_timeout_seconds: Some(86_400),
            session_tickets: false,
            prefer_server_ciphers: false,
            backend: None,
            key_exchange_groups: None,
            ktls: KtlsMode::Auto,
        }
    );
}

#[test]
fn lowers_exact_ip_server_names_as_canonical_certificate_identities_without_reading_files() {
    let report = import_source(
        r"http {
          server {
            listen 192.0.2.10:8443 ssl default_server;
            server_name 192.0.2.10;
            ssl_certificate /definitely/missing/shared-chain.pem;
            ssl_certificate_key /definitely/missing/shared-key.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            location / { return 204; }
          }
          server {
            listen [2001:db8::1]:8443 ssl default_server;
            server_name 2001:0DB8:0:0:0:0:0:1;
            ssl_certificate /definitely/missing/shared-chain.pem;
            ssl_certificate_key /definitely/missing/shared-key.pem;
            ssl_protocols TLSv1.2 TLSv1.3;
            location / { return 204; }
          }
        }",
    );

    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("IP-bound TLS listeners");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(
        config.certificates[0].dns_names,
        ["192.0.2.10", "2001:db8::1"]
    );
    assert_eq!(config.listeners.len(), 2);
    assert!(config.listeners.iter().all(|listener| {
        let profile = listener.tls_profile.as_ref().expect("TLS profile");
        config
            .tls_profiles
            .iter()
            .find(|candidate| &candidate.name == profile)
            .is_some_and(|profile| profile.default_certificate == config.certificates[0].name)
    }));
}
