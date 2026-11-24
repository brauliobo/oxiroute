use std::fmt::Write as _;
use std::path::Path;

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_NODES, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH, decode_value, expand_templates, render_config, resolve_source,
    resolve_source_with_format,
};
use serde_json::json;

#[test]
fn infers_toml_extension_case_insensitively() {
    assert_eq!(
        ConfigFormat::infer(Path::new("config.toml")).unwrap(),
        ConfigFormat::Toml
    );
    assert_eq!(
        ConfigFormat::infer(Path::new("config.TOML")).unwrap(),
        ConfigFormat::Toml
    );
}

#[test]
fn decodes_generic_values_preserving_integers_and_floats() {
    let source = br#"
title = "oxiroute"
enabled = true
disabled = false
count = 42
negative = -7
zero = 0
ratio = 1.5
whole_float = 2.0
tags = ["edge", "proxy"]
numbers = [1, 2, 3]

[nested]
host = "127.0.0.1"
port = 8080

[[items]]
name = "first"
weight = 10

[[items]]
name = "second"
weight = 20
"#;
    let decoded = decode_value(ConfigFormat::Toml, source).unwrap();
    assert_eq!(
        decoded,
        json!({
            "title": "oxiroute",
            "enabled": true,
            "disabled": false,
            "count": 42,
            "negative": -7,
            "zero": 0,
            "ratio": 1.5,
            "whole_float": 2.0,
            "tags": ["edge", "proxy"],
            "numbers": [1, 2, 3],
            "nested": {
                "host": "127.0.0.1",
                "port": 8080
            },
            "items": [
                {"name": "first", "weight": 10},
                {"name": "second", "weight": 20}
            ]
        })
    );
    assert!(decoded["count"].is_i64());
    assert!(decoded["negative"].is_i64());
    assert!(decoded["zero"].is_i64());
    assert!(decoded["ratio"].is_f64());
    assert!(!decoded["ratio"].is_i64());
    assert!(decoded["whole_float"].is_f64());
    assert!(!decoded["whole_float"].is_i64());
}

fn full_validated_config() -> oxiroute_config::ValidatedConfig {
    let draft: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listener_workers": 4,
        "certificates": [{
            "name": "edge-cert",
            "dns_names": ["api.example.test", "www.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/etc/oxiroute/edge-chain.pem",
                "private_key_path": "/etc/oxiroute/edge-key.pem"
            }
        }],
        "tls_profiles": [
            {
                "name": "edge-tls",
                "certificates": ["edge-cert"],
                "default_certificate": "edge-cert",
                "min_version": "1.3",
                "alpn": ["h2", "http/1.1"]
            },
            {
                "name": "forward-tls",
                "certificates": ["edge-cert"],
                "default_certificate": "edge-cert",
                "min_version": "1.3",
                "alpn": ["h3"]
            }
        ],
        "listeners": [
            {
                "name": "https-edge",
                "bind": {"type": "socket", "address": "127.0.0.1:8443"},
                "protocol": "http",
                "service": "web",
                "tls_profile": "edge-tls"
            },
            {
                "name": "forward-http3",
                "bind": {"type": "udp", "address": "127.0.0.1:9443"},
                "protocol": "forward_http3",
                "service": "egress",
                "tls_profile": "forward-tls"
            }
        ],
        "cache_stores": [
            {"name": "memory", "type": "memory"}
        ],
        "upstream_pools": [{
            "name": "backend",
            "endpoints": [
                {"type": "dns", "host": "origin.example.test", "port": 8080}
            ]
        }],
        "http_services": [{
            "name": "web",
            "routes": [
                {
                    "host": {"kind": "normalized_host", "value": "api.example.test"},
                    "path": {"kind": "segment_prefix", "value": "/api"},
                    "action": {
                        "type": "proxy",
                        "upstream_pool": "backend",
                        "policy": {}
                    }
                },
                {
                    "path": {"kind": "exact", "value": "/healthz"},
                    "action": {
                        "type": "fixed_response",
                        "status": 200,
                        "body": "ok"
                    }
                }
            ]
        }],
        "forward_proxy_services": [{
            "name": "egress",
            "enabled_versions": ["h1", "h2", "h3"],
            "cache": {"store": "memory"}
        }]
    }))
    .expect("valid authored ConfigDraft");
    draft.validate().expect("validated ConfigDraft")
}

#[test]
fn full_validated_config_round_trips_through_toml() {
    let validated = full_validated_config();

    let rendered = render_config(ConfigFormat::Toml, &validated).expect("render TOML");
    assert!(rendered.ends_with('\n'));
    assert!(!rendered.contains("\r\n"));

    let resolved_by_ext = resolve_source(Path::new("oxiroute.toml"), rendered.as_bytes())
        .expect("resolve_source with .toml extension");
    assert_eq!(resolved_by_ext.format, ConfigFormat::Toml);
    assert_eq!(resolved_by_ext.config, validated);

    let resolved_explicit =
        resolve_source_with_format(Path::new("config"), rendered.as_bytes(), ConfigFormat::Toml)
            .expect("resolve_source_with_format with ConfigFormat::Toml");
    assert_eq!(resolved_explicit.format, ConfigFormat::Toml);
    assert_eq!(resolved_explicit.config, validated);

    // Deterministic re-rendering produces byte-identical output.
    let rerendered =
        render_config(ConfigFormat::Toml, &resolved_by_ext.config).expect("re-render TOML");
    assert_eq!(rendered, rerendered);
}

#[test]
fn expands_generic_templates_and_use_in_toml() {
    let source = br#"
version = 1

[templates.tcp_listener]
protocol = "http"
service = "web"

[templates.health_route.action]
type = "fixed_response"
status = 200
body = "healthy"

[[listeners]]
use = "tcp_listener"
name = "http-edge"
bind = { type = "socket", address = "127.0.0.1:8080" }

[[http_services]]
name = "web"

[[http_services.routes]]
use = "health_route"
path = { kind = "exact", value = "/healthz" }
"#;

    let decoded = decode_value(ConfigFormat::Toml, source).expect("decode TOML");
    let expanded = expand_templates(&decoded).expect("expand templates");
    assert!(expanded.get("templates").is_none());
    assert_eq!(expanded["listeners"][0]["protocol"], json!("http"));
    assert_eq!(
        expanded["http_services"][0]["routes"][0]["action"]["status"],
        json!(200)
    );

    let resolved =
        resolve_source(Path::new("templated.toml"), source).expect("resolve TOML with templates");
    let expected = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [{
            "name": "http-edge",
            "bind": {"type": "socket", "address": "127.0.0.1:8080"},
            "protocol": "http",
            "service": "web"
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": {"kind": "exact", "value": "/healthz"},
                "action": {
                    "type": "fixed_response",
                    "status": 200,
                    "body": "healthy"
                }
            }]
        }]
    }))
    .expect("expected ConfigDraft")
    .validate()
    .expect("validated expected ConfigDraft");
    assert_eq!(resolved.format, ConfigFormat::Toml);
    assert_eq!(resolved.config, expected);
}

#[test]
fn rejects_duplicate_keys() {
    let source = b"version = 1\nversion = 2\n";
    let error = decode_value(ConfigFormat::Toml, source).unwrap_err();
    assert!(
        matches!(
            error,
            ConfigSourceError::Parse {
                format: "TOML",
                line: Some(2),
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn rejects_invalid_syntax_with_one_based_line_number() {
    let source = b"valid = 1\nstill_valid = true\nbroken = [1, 2\n";
    let error = decode_value(ConfigFormat::Toml, source).unwrap_err();
    assert!(
        matches!(
            error,
            ConfigSourceError::Parse {
                format: "TOML",
                line: Some(3),
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn rejects_non_finite_floats() {
    for source in [
        b"value = nan\n".as_slice(),
        b"value = +nan\n".as_slice(),
        b"value = -nan\n".as_slice(),
        b"value = inf\n".as_slice(),
        b"value = +inf\n".as_slice(),
        b"value = -inf\n".as_slice(),
    ] {
        let error = decode_value(ConfigFormat::Toml, source).unwrap_err();
        assert!(
            matches!(error, ConfigSourceError::Parse { format: "TOML", .. }),
            "unexpected error for {:?}: {error:?}",
            std::str::from_utf8(source)
        );
    }
}

#[test]
fn rejects_toml_datetime_values() {
    for source in [
        b"ts = 2026-11-24T12:30:00Z\n".as_slice(),
        b"ts = 2026-11-24T12:30:00\n".as_slice(),
        b"ts = 2026-11-24\n".as_slice(),
        b"ts = 12:30:00\n".as_slice(),
        b"items = [2026-11-24]\n".as_slice(),
    ] {
        let error = decode_value(ConfigFormat::Toml, source).unwrap_err();
        assert!(
            matches!(error, ConfigSourceError::Parse { format: "TOML", .. }),
            "unexpected error for {:?}: {error:?}",
            std::str::from_utf8(source)
        );
    }
}

#[test]
fn enforces_structural_depth_limit() {
    // Use dotted table header to exceed MAX_STRUCTURAL_DEPTH cleanly.
    let mut header = String::from("[");
    for index in 0..=MAX_STRUCTURAL_DEPTH {
        if index > 0 {
            header.push('.');
        }
        write!(header, "k{index}").unwrap();
    }
    header.push_str("]\nleaf = 1\n");

    let error = decode_value(ConfigFormat::Toml, header.as_bytes()).unwrap_err();
    assert!(
        matches!(error, ConfigSourceError::StructuralDepth),
        "unexpected error: {error:?}"
    );
}

#[test]
fn enforces_node_limit() {
    // Each element in an array is 1 node, and "0," is only 2 bytes, staying well under MAX_SOURCE_BYTES.
    let mut source = String::from("a = [");
    for index in 0..=MAX_NODES {
        if index > 0 {
            source.push(',');
        }
        source.push('0');
    }
    source.push_str("]\n");
    assert!(source.len() <= MAX_SOURCE_BYTES);

    let error = decode_value(ConfigFormat::Toml, source.as_bytes()).unwrap_err();
    assert!(
        matches!(error, ConfigSourceError::NodeLimit),
        "unexpected error: {error:?}"
    );
}

#[test]
fn enforces_string_limit() {
    let oversized = "a".repeat(MAX_STRING_BYTES + 1);
    let source = format!("s = \"{oversized}\"\n");
    assert!(source.len() <= MAX_SOURCE_BYTES);

    let error = decode_value(ConfigFormat::Toml, source.as_bytes()).unwrap_err();
    assert!(
        matches!(error, ConfigSourceError::StringLimit),
        "unexpected error: {error:?}"
    );
}

#[test]
fn enforces_source_size_limit() {
    let oversized = vec![b' '; MAX_SOURCE_BYTES + 1];
    assert!(matches!(
        decode_value(ConfigFormat::Toml, &oversized),
        Err(ConfigSourceError::SourceTooLarge)
    ));
}
