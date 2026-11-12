use std::path::Path;

use oxiroute_config::{ConfigDraft, TlsBackend, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::{Value, json};

const FORMATS: [ConfigFormat; 4] = [
    ConfigFormat::Kdl,
    ConfigFormat::Lua,
    ConfigFormat::Uci,
    ConfigFormat::Hocon,
];

fn config(policy: &Value) -> ValidatedConfig {
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "certificates": [{
            "name": "server",
            "dns_names": ["server.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/etc/oxiroute/server-chain.pem",
                "private_key_path": "/etc/oxiroute/server-key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "web",
            "certificates": ["server"],
            "default_certificate": "server",
            "policy": policy
        }],
        "listeners": []
    }))
    .expect("authored profile")
    .validate()
    .expect("valid TLS profile")
}

fn round_trip(config: &ValidatedConfig, format: ConfigFormat) -> ValidatedConfig {
    let rendered = render_config(format, config).expect("render");
    resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
        .unwrap_or_else(|error| panic!("{format:?}: {error}\n{rendered}"))
        .config
}

#[test]
fn the_rustls_backend_and_the_policy_it_shares_with_openssl_round_trip_through_every_format() {
    let written = config(&json!({
        "backend": "rustls",
        "cipher_list": "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:HIGH:!aNULL",
        "dh_parameters_path": "/etc/oxiroute/dhparam.pem",
        "prefer_server_ciphers": false,
        "session_tickets": true,
        "session_timeout_seconds": 600,
        "session_cache": {"name": "shared", "size_bytes": 1_048_576},
        "key_exchange_groups": ["x25519", "secp256r1"],
        "ktls": "off",
    }));
    assert_eq!(
        written.as_draft().tls_profiles[0].policy.backend,
        TlsBackend::Rustls
    );
    for format in FORMATS {
        assert_eq!(round_trip(&written, format), written, "{format:?}");
    }
}

#[test]
fn a_profile_without_a_backend_renders_the_default_and_reads_it_back() {
    let written = config(&json!({}));
    for format in FORMATS {
        let rendered = render_config(format, &written).expect("render");
        assert!(rendered.contains("backend"), "{format:?}: {rendered}");
        assert_eq!(
            round_trip(&written, format).as_draft().tls_profiles[0]
                .policy
                .backend,
            TlsBackend::default(),
            "{format:?}"
        );
    }
}
