use std::{fs, path::PathBuf};

use oxiroute_config::Protocol;
use tempfile::tempdir;

use super::common::{analyze_fixture, assert_has_provenance, diagnostic_contains, import_fixture};

#[test]
fn tls_bind_retains_pem_san_identities_sidecar_key_and_downstream_timeout() {
    let certificate_path = fixture_path("tls-chain.pem");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout client 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display()
    );
    let resolved = analyze_fixture("tls.cfg", source.as_bytes());
    let tls = resolved.value().frontends[0].binds[0]
        .tls
        .as_ref()
        .expect("resolved TLS bind");
    assert_eq!(tls.value.dns_names, ["proxy.example.test"]);
    assert_eq!(tls.value.certificate_chain_path, certificate_path);
    assert_eq!(
        tls.value.private_key_path,
        certificate_path.with_file_name("tls-chain.pem.key")
    );

    let lowered = import_fixture("tls.cfg", source.as_bytes());
    let candidate = lowered.value();

    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("TLS config with client timeout");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(
        config.listeners[0].downstream_timeouts.client_timeout_ms,
        Some(30_000)
    );
}

#[test]
fn exact_http_tls_default_route_finalizes_with_an_unbounded_body_policy() {
    let certificate_path = fixture_path("tls-chain.pem");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display()
    );
    let lowered = import_fixture("strict-http-tls.cfg", source.as_bytes());

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized strict HTTP TLS");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.listeners[0].protocol, Protocol::Http);
    assert!(config.listeners[0].tls_profile.is_some());
    assert_eq!(config.http_services[0].max_request_body_bytes, None);
    for path in ["/certificates/0", "/tls_profiles/0", "/listeners/0"] {
        assert_has_provenance(lowered.value(), path);
    }
}

#[test]
fn tls_sidecar_key_must_match_the_leaf_certificate() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempdir().expect("TLS identity directory");
    let certificate_path = directory.path().join("proxy.pem");
    let private_key_path = directory.path().join("proxy.pem.key");
    fs::copy(fixture_path("tls-chain.pem"), &certificate_path).expect("copy certificate chain");
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nginx/proxy-mismatched-key.pem"),
        &private_key_path,
    )
    .expect("copy mismatched key");
    fs::set_permissions(&private_key_path, fs::Permissions::from_mode(0o600))
        .expect("secure mismatched key mode");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display()
    );

    let lowered = import_fixture("mismatched-tls.cfg", source.as_bytes());
    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().certificates, 0);
    assert!(diagnostic_contains(
        lowered.diagnostics(),
        "does not match the leaf certificate"
    ));
}

#[test]
fn repeated_tls_bundle_is_deduplicated_across_canonical_listeners() {
    let certificate_path = fixture_path("tls-chain.pem");
    let source = format!(
        "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout client 30s\n  timeout server 30s\nfrontend first\n  bind 127.0.0.1:8443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nfrontend second\n  bind 127.0.0.1:9443 ssl crt {} alpn h2,http/1.1\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app1 127.0.0.1:3000\n",
        certificate_path.display(),
        certificate_path.display()
    );
    let resolved = analyze_fixture("reused-tls.cfg", source.as_bytes());
    let first = resolved.value().frontends[0].binds[0]
        .tls
        .as_ref()
        .expect("first TLS bind");
    let second = resolved.value().frontends[1].binds[0]
        .tls
        .as_ref()
        .expect("second TLS bind");
    assert_eq!(first.value, second.value);

    let lowered = import_fixture("reused-tls.cfg", source.as_bytes());
    let candidate = lowered.value();

    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("reused TLS config");
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(config.tls_profiles.len(), 2);
    assert_eq!(config.listeners.len(), 2);
    assert!(
        candidate
            .provenance
            .iter()
            .any(|provenance| provenance.path.starts_with("/certificates/"))
    );
}

#[test]
fn tls_bind_with_no_dns_identities_never_emits_a_listener_or_empty_certificate() {
    let certificate_path = fixture_path("tls-no-identities.pem");
    let source = format!(
        "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn http/1.1\n  maxconn 100\n",
        certificate_path.display()
    );
    let lowered = import_fixture("tls-empty-identities.cfg", source.as_bytes());

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(lowered.value().summary().certificates, 0);
    assert_eq!(lowered.value().summary().tls_profiles, 0);
    assert_eq!(lowered.value().summary().listeners, 0);
    assert!(diagnostic_contains(
        lowered.diagnostics(),
        "no DNS subject alternative names"
    ));
}

#[test]
fn crt_list_and_multiple_crt_parameters_are_blocked_without_guessing() {
    let certificate_path = fixture_path("tls-chain.pem");
    let sources = [
        format!(
            "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt-list {}\n",
            certificate_path.display()
        ),
        format!(
            "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} crt {}\n",
            certificate_path.display(),
            certificate_path.display()
        ),
    ];

    for source in sources {
        let lowered = import_fixture("unsupported-certs.cfg", source.as_bytes());
        assert!(
            lowered
                .value()
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none()
        );
        assert_eq!(lowered.value().summary().certificates, 0);
        assert_eq!(lowered.value().summary().listeners, 0);
        assert!(diagnostic_contains(
            lowered.diagnostics(),
            "certificate selection"
        ));
    }
}

#[test]
fn oversized_certificate_metadata_is_blocked_before_a_tls_listener_is_emitted() {
    let temp = tempdir().expect("temporary directory");
    let certificate_path = temp.path().join("oversized.pem");
    fs::write(&certificate_path, vec![b'x'; 1024 * 1024 + 1]).expect("oversized certificate");
    let source = format!(
        "defaults web\n  mode http\nfrontend public\n  bind 127.0.0.1:8443 ssl crt {} alpn http/1.1\n  maxconn 100\n",
        certificate_path.display()
    );
    let lowered = import_fixture("oversized-tls.cfg", source.as_bytes());

    assert_eq!(lowered.value().summary().certificates, 0);
    assert_eq!(lowered.value().summary().listeners, 0);
    assert!(diagnostic_contains(
        lowered.diagnostics(),
        "exceeds 1048576 bytes"
    ));
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/haproxy")
        .join(name)
        .canonicalize()
        .expect("canonical fixture path")
}
