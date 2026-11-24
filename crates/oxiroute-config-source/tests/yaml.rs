use std::fmt::Write as _;
use std::path::Path;

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_NODES, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH, decode_value, render_config, resolve_source, resolve_source_with_format,
};
use serde_json::{Value, json};

#[test]
fn infers_yaml_and_yml_extensions_case_insensitively() {
    for path in ["config.yaml", "config.yml", "config.YAML", "config.YML"] {
        assert_eq!(ConfigFormat::infer(path).unwrap(), ConfigFormat::Yaml);
    }
}

#[test]
fn decodes_and_resolves_yaml_and_yml_sources() {
    let source = br"
version: 1
listeners: []
listener_workers: 4
connection_scheduling: aged_per_worker
worker_scaling: dynamic
min_active_workers: 2
target_busy_percent: 60
scale_down_delay_ms: 2500
";

    let expected_json = json!({
        "version": 1,
        "listeners": [],
        "listener_workers": 4,
        "connection_scheduling": "aged_per_worker",
        "worker_scaling": "dynamic",
        "min_active_workers": 2,
        "target_busy_percent": 60,
        "scale_down_delay_ms": 2500
    });
    let expected_config = serde_json::from_value::<ConfigDraft>(expected_json.clone())
        .unwrap()
        .validate()
        .unwrap();

    let decoded = decode_value(ConfigFormat::Yaml, source).unwrap();
    assert_eq!(decoded, expected_json);

    for filename in ["oxiroute.yaml", "oxiroute.yml"] {
        let resolved = resolve_source(Path::new(filename), source).unwrap();
        assert_eq!(resolved.format, ConfigFormat::Yaml);
        assert!(!resolved.compositional);
        assert_eq!(resolved.config, expected_config);
    }

    let explicit =
        resolve_source_with_format(Path::new("config"), source, ConfigFormat::Yaml).unwrap();
    assert_eq!(explicit.format, ConfigFormat::Yaml);
    assert_eq!(explicit.config, expected_config);
}

#[test]
fn round_trips_full_validated_config_through_yaml() {
    let draft: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listener_workers": 4,
        "certificates": [{
            "name": "edge-cert",
            "dns_names": ["proxy.example.test"],
            "source": {
                "type": "files",
                "certificate_chain_path": "/etc/oxiroute/chain.pem",
                "private_key_path": "/etc/oxiroute/key.pem"
            }
        }],
        "tls_profiles": [{
            "name": "edge-tls",
            "certificates": ["edge-cert"],
            "default_certificate": "edge-cert",
            "min_version": "1.3",
            "alpn": ["h2", "http/1.1"]
        }],
        "listeners": [{
            "name": "https-in",
            "bind": {"type": "socket", "address": "127.0.0.1:8443"},
            "protocol": "http",
            "service": "web",
            "tls_profile": "edge-tls"
        }],
        "cache_stores": [{"name": "memory", "type": "memory"}],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{"type": "dns", "host": "origin.example.test", "port": 443}],
            "tls": {"verify": "none", "server_name": "origin.example.test"}
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": {"kind": "segment_prefix", "value": "/"},
                "action": {
                    "type": "proxy",
                    "upstream_pool": "origin",
                    "policy": {
                        "upstream_host": {"type": "nginx_host", "fallback": ""},
                        "request_headers": [{
                            "operation": "set",
                            "name": "x-host",
                            "value": {"type": "nginx_host", "fallback": ".example.org"}
                        }],
                        "response_cookie_path_rewrites": [{
                            "from": "/",
                            "to": "/; SameSite=none; Secure",
                            "mode": "nginx_prefix"
                        }]
                    }
                }
            }]
        }]
    }))
    .expect("valid draft");

    let validated = draft.validate().expect("validated config");
    let rendered = render_config(ConfigFormat::Yaml, &validated).expect("YAML render");
    assert!(rendered.ends_with('\n'));
    assert!(!rendered.contains('\r'));

    let resolved =
        resolve_source(Path::new("config.yaml"), rendered.as_bytes()).expect("YAML resolve");
    assert_eq!(resolved.config, validated);

    // Deterministic rendering across repeated passes.
    let rerendered = render_config(ConfigFormat::Yaml, &resolved.config).expect("second render");
    assert_eq!(rendered, rerendered);
}

#[test]
fn preserves_scalar_types_and_special_strings_in_yaml() {
    let source = r#"
int_pos: 42
int_neg: -7
int_zero: 0
int_hex: 0x10
int_oct: 0o10
float_val: 3.5
float_whole: 2.0
bool_true: true
bool_false: false
null_word: null
null_tilde: ~
str_true: "true"
str_false: "FALSE"
str_null: "null"
str_tilde: "~"
str_int: "123"
str_float: "1.3"
str_nan: ".nan"
str_inf: ".inf"
str_empty: ""
str_spaces: "  padded  "
str_multiline: "line1\nline2\tindented"
str_indicators: "-foo: bar # [1, 2] {a: b} *x &y !z"
str_unicode: "café 日本語"
empty_map: {}
empty_list: []
nested_arrays:
  - - a: 1
      b: [true, false]
    - {}
  - []
plain_with_amp_and_star: a & b * c
"#
    .as_bytes();

    let decoded = decode_value(ConfigFormat::Yaml, source).unwrap();
    assert_eq!(decoded["int_pos"].as_i64(), Some(42));
    assert_eq!(decoded["int_neg"].as_i64(), Some(-7));
    assert_eq!(decoded["int_zero"].as_i64(), Some(0));
    assert_eq!(decoded["int_hex"].as_i64(), Some(16));
    assert_eq!(decoded["int_oct"].as_i64(), Some(8));
    assert_eq!(decoded["float_val"].as_f64(), Some(3.5));
    assert!(decoded["float_val"].is_f64());
    assert_eq!(decoded["float_whole"].as_f64(), Some(2.0));
    assert!(decoded["float_whole"].is_f64());
    assert!(decoded["float_whole"].as_i64().is_none());
    assert_eq!(decoded["bool_true"], Value::Bool(true));
    assert_eq!(decoded["bool_false"], Value::Bool(false));
    assert_eq!(decoded["null_word"], Value::Null);
    assert_eq!(decoded["null_tilde"], Value::Null);
    assert_eq!(decoded["str_true"], json!("true"));
    assert_eq!(decoded["str_false"], json!("FALSE"));
    assert_eq!(decoded["str_null"], json!("null"));
    assert_eq!(decoded["str_tilde"], json!("~"));
    assert_eq!(decoded["str_int"], json!("123"));
    assert_eq!(decoded["str_float"], json!("1.3"));
    assert_eq!(decoded["str_nan"], json!(".nan"));
    assert_eq!(decoded["str_inf"], json!(".inf"));
    assert_eq!(decoded["str_empty"], json!(""));
    assert_eq!(decoded["str_spaces"], json!("  padded  "));
    assert_eq!(decoded["str_multiline"], json!("line1\nline2\tindented"));
    assert_eq!(
        decoded["str_indicators"],
        json!("-foo: bar # [1, 2] {a: b} *x &y !z")
    );
    assert_eq!(decoded["str_unicode"], json!("café 日本語"));
    assert_eq!(decoded["empty_map"], json!({}));
    assert_eq!(decoded["empty_list"], json!([]));
    assert_eq!(
        decoded["nested_arrays"],
        json!([[{"a": 1, "b": [true, false]}, {}], []])
    );
    assert_eq!(decoded["plain_with_amp_and_star"], json!("a & b * c"));
}

#[test]
fn expands_generic_templates_and_use_in_yaml() {
    let source = br"
templates:
  base_pool:
    endpoints:
      - type: dns
        host: origin.example.test
        port: 443
    tls:
      verify: none
version: 1
listeners: []
upstream_pools:
  - use: base_pool
    name: primary
";

    let expected_config = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": [{
            "name": "primary",
            "endpoints": [{
                "type": "dns",
                "host": "origin.example.test",
                "port": 443
            }],
            "tls": {"verify": "none"}
        }]
    }))
    .unwrap()
    .validate()
    .unwrap();

    let resolved = resolve_source(Path::new("config.yaml"), source).unwrap();
    assert_eq!(resolved.format, ConfigFormat::Yaml);
    assert!(resolved.compositional);
    assert_eq!(resolved.config, expected_config);
}

#[test]
fn rejects_empty_and_multi_document_yaml() {
    for empty in ["", "   \n\t\n", "# only comments\n", "---\n", "---\n...\n"] {
        let error = decode_value(ConfigFormat::Yaml, empty.as_bytes()).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigSourceError::Parse {
                    format: "YAML",
                    ..
                }
            ),
            "expected YAML parse error for {empty:?}, got {error}"
        );
    }

    let multi = b"a: 1\n---\nb: 2\n";
    let error = decode_value(ConfigFormat::Yaml, multi).unwrap_err();
    assert!(
        matches!(
            error,
            ConfigSourceError::Parse {
                format: "YAML",
                ..
            }
        ) && error.to_string().contains("multi-document"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_duplicate_mapping_keys() {
    let source = b"a: 1\nb: 2\na: 3\n";
    let error = decode_value(ConfigFormat::Yaml, source).unwrap_err();
    assert!(
        matches!(
            error,
            ConfigSourceError::Parse {
                format: "YAML",
                ..
            }
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_non_string_mapping_keys() {
    let cases = [
        "1: one\n",
        "true: yes\n",
        "null: nothing\n",
        "1.5: float\n",
        "[1, 2]: seq\n",
        "{a: 1}: map\n",
    ];
    for source in cases {
        let error = decode_value(ConfigFormat::Yaml, source.as_bytes()).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigSourceError::Parse {
                    format: "YAML",
                    ..
                }
            ) && error.to_string().contains("mapping keys must be strings"),
            "unexpected error for {source:?}: {error}"
        );
    }
}

#[test]
fn rejects_non_finite_floats() {
    for source in [
        "value: .nan\n",
        "value: .NaN\n",
        "value: .inf\n",
        "value: +.inf\n",
        "value: -.inf\n",
    ] {
        let error = decode_value(ConfigFormat::Yaml, source.as_bytes()).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigSourceError::Parse {
                    format: "YAML",
                    ..
                }
            ) && error.to_string().contains("non-finite"),
            "unexpected error for {source:?}: {error}"
        );
    }
}

#[test]
fn rejects_yaml_anchors_aliases_and_tags() {
    let cases = [
        "a: &anchor 1\nb: *anchor\n",
        "a: &unused 1\n",
        "a: &seq\n  - 1\n  - 2\n",
        "a: &map\n  k: v\n",
        "&k foo: bar\n",
        "a: !custom 123\n",
        "a: !!str 123\n",
        // Billion-laughs style exponential alias expansion attempt.
        concat!(
            "a: &a [\"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\"]\n",
            "b: &b [*a, *a, *a, *a, *a, *a, *a, *a, *a]\n",
            "c: &c [*b, *b, *b, *b, *b, *b, *b, *b, *b]\n",
            "d: &d [*c, *c, *c, *c, *c, *c, *c, *c, *c]\n",
            "e: &e [*d, *d, *d, *d, *d, *d, *d, *d, *d]\n",
            "f: &f [*e, *e, *e, *e, *e, *e, *e, *e, *e]\n",
        ),
    ];

    for source in cases {
        let error = decode_value(ConfigFormat::Yaml, source.as_bytes()).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigSourceError::Parse {
                    format: "YAML",
                    ..
                }
            ),
            "expected YAML parse error for {source:?}, got {error}"
        );
    }
}

#[test]
fn enforces_structural_depth_node_string_and_source_limits() {
    // Structural depth limit.
    let mut deep = String::new();
    for _ in 0..=MAX_STRUCTURAL_DEPTH {
        deep.push('[');
    }
    deep.push('1');
    for _ in 0..=MAX_STRUCTURAL_DEPTH {
        deep.push(']');
    }
    assert!(matches!(
        decode_value(ConfigFormat::Yaml, deep.as_bytes()),
        Err(ConfigSourceError::StructuralDepth)
    ));

    // String byte limit.
    let oversized_string = format!("value: {}\n", "a".repeat(MAX_STRING_BYTES + 1));
    assert!(matches!(
        decode_value(ConfigFormat::Yaml, oversized_string.as_bytes()),
        Err(ConfigSourceError::StringLimit)
    ));

    // Source byte limit.
    let oversized_source = vec![b' '; MAX_SOURCE_BYTES + 1];
    assert!(matches!(
        decode_value(ConfigFormat::Yaml, &oversized_source),
        Err(ConfigSourceError::SourceTooLarge)
    ));

    // Node limit (root array + MAX_NODES elements = MAX_NODES + 1 nodes).
    let mut many_nodes = String::with_capacity((MAX_NODES + 1) * 2 + 2);
    many_nodes.push('[');
    for index in 0..MAX_NODES {
        if index > 0 {
            many_nodes.push(',');
        }
        write!(many_nodes, "1").unwrap();
    }
    many_nodes.push(']');
    assert!(matches!(
        decode_value(ConfigFormat::Yaml, many_nodes.as_bytes()),
        Err(ConfigSourceError::NodeLimit)
    ));
}
