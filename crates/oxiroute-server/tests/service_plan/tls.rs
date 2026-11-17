use std::sync::Arc;

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ConfigError, Protocol, TlsProfile, TlsVersion,
    UpstreamTls,
};
use tempfile::TempDir;

use crate::canonical::canonical_config;
use crate::config_support::service_specs;
use crate::fixture_support::write_test_identity;

#[test]
fn compiles_and_retains_one_shared_listener_tls_profile() {
    let temp = TempDir::new().expect("TLS temp directory");
    let (certificate_chain_path, private_key_path) =
        write_test_identity(temp.path(), "test-only-private-key.pem");
    let mut config = canonical_config();
    config.certificates.push(Certificate {
        name: "public".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Files {
            certificate_chain_path,
            private_key_path,
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "public".into(),
        certificates: vec!["public".into()],
        default_certificate: "public".into(),
        min_version: TlsVersion::Tls12,
        alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.listeners[0].tls_profile = Some("public".into());
    config.listeners[1].tls_profile = Some("public".into());

    let validated = config.validate().unwrap();
    let services = service_specs(&validated).expect("TLS services");
    let tls = oxiroute_server::tls::prepare_tls(&validated).expect("prepared TLS");
    let first = services[0].tls.as_ref().expect("first TLS profile");
    let second = services[1].tls.as_ref().expect("second TLS profile");
    let prepared = tls.profiles().get("public").expect("prepared profile");

    assert!(Arc::ptr_eq(first, second));
    assert_eq!(first.name(), "public");
    assert_eq!(first.name(), prepared.name());
    assert_eq!(first.min_version(), TlsVersion::Tls12);
    assert!(first.tls_settings(0).is_ok());
    assert_eq!(tls.certificates().len(), 1);
    assert_eq!(tls.profiles().len(), 1);
}

#[test]
fn rejects_an_unknown_programmatic_listener_tls_profile() {
    let mut config = canonical_config();
    config.listeners[0].tls_profile = Some("missing".into());

    assert!(matches!(
        config.validate(),
        Err(ConfigError::UnknownListenerTlsProfile { listener, profile })
            if listener == "web" && profile == "missing"
    ));
}

#[test]
fn rejects_tls_profiles_on_programmatic_tcp_and_rtmp_listeners() {
    for (listener_index, expected_protocol) in [(2, Protocol::Tcp), (3, Protocol::Rtmp)] {
        let mut config = canonical_config();
        config.listeners[listener_index].tls_profile = Some("not-allowed".into());

        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnexpectedListenerTlsProfile { protocol, .. })
                if protocol == expected_protocol
        ));
    }
}

#[test]
fn rejects_a_tls_upstream_pool_for_a_programmatic_l4_service() {
    let mut config = canonical_config();
    config.upstream_pools[1].tls = Some(UpstreamTls {
        server_name: Some("database.example.test".into()),
        verify: oxiroute_config::UpstreamTlsVerify::Full,
        ca_certificate_path: None,
    });

    assert!(matches!(
        config.validate(),
        Err(ConfigError::TlsUpstreamPoolForL4Service { service, pool })
            if service == "database" && pool == "database"
    ));
}

#[test]
fn rejects_unvalidated_programmatic_certificate_paths() {
    let mut config = canonical_config();
    config.certificates.push(Certificate {
        name: "public".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Files {
            certificate_chain_path: "relative-chain.pem".into(),
            private_key_path: "/private/key.pem".into(),
        },
    });

    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidFilePath {
            kind: "certificate",
            name,
            field: "source.certificate_chain_path",
            ..
        }) if name == "public"
    ));
}
