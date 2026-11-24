use std::path::Path;

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_EXPANSION_DEPTH, MAX_OUTPUT_BYTES, MAX_STRING_BYTES,
    MAX_SUBSTITUTIONS, expand_templates, resolve_source_with_format,
};
use serde_json::{Map, Value, json};

#[test]
fn recursively_merges_templates_with_local_precedence() {
    let source = json!({
        "templates": {
            "base": {
                "nested": {"first": 1, "replace": "base"},
                "array": [1, 2],
                "nullable": "base"
            },
            "derived": {
                "use": "base",
                "nested": {"second": 2, "replace": "derived"},
                "array": [3]
            },
            "last": {"nested": {"third": 3}, "last": true}
        },
        "item": {
            "use": ["derived", "last"],
            "nested": {"replace": "local"},
            "nullable": null
        }
    });
    assert_eq!(
        expand_templates(&source).unwrap(),
        json!({
            "item": {
                "nested": {
                    "first": 1,
                    "second": 2,
                    "third": 3,
                    "replace": "local"
                },
                "array": [3],
                "nullable": null,
                "last": true
            }
        })
    );
}

#[test]
fn expands_nested_uses_and_removes_every_marker() {
    let source = json!({
        "templates": {"base": {"enabled": true}},
        "outer": {
            "inner": {"use": "base", "name": "inside"},
            "items": [{"use": "base"}]
        }
    });
    let expanded = expand_templates(&source).unwrap();
    assert_eq!(
        expanded,
        json!({
            "outer": {
                "inner": {"enabled": true, "name": "inside"},
                "items": [{"enabled": true}]
            }
        })
    );
    assert!(!expanded.to_string().contains("\"use\""));
    assert!(!expanded.to_string().contains("\"templates\""));
}

#[test]
fn rejects_unknown_templates_cycles_and_invalid_use_shapes() {
    let invalid = [
        json!({"item": {"use": "missing"}}),
        json!({
            "templates": {"a": {"use": "b"}, "b": {"use": "a"}},
            "item": {"use": "a"}
        }),
        json!({"templates": {"bad": 1}, "item": {"use": "bad"}}),
        json!({"templates": [], "item": {}}),
        json!({"item": {"use": 1}}),
        json!({"item": {"use": ["valid", 2]}}),
    ];
    for source in invalid {
        assert!(matches!(
            expand_templates(&source),
            Err(ConfigSourceError::Template(_))
        ));
    }
}

#[test]
fn applies_the_inheritance_depth_bound() {
    let mut templates = Map::new();
    for index in 0..=MAX_EXPANSION_DEPTH {
        let value = if index == MAX_EXPANSION_DEPTH {
            json!({"value": true})
        } else {
            json!({"use": format!("template-{}", index + 1)})
        };
        templates.insert(format!("template-{index}"), value);
    }
    let source = json!({
        "templates": templates,
        "item": {"use": "template-0"}
    });
    assert!(matches!(
        expand_templates(&source),
        Err(ConfigSourceError::ExpansionDepth)
    ));
}

#[test]
fn bounds_expanded_output_independently_of_input() {
    let chunk = "x".repeat(MAX_STRING_BYTES);
    let mut root = Map::new();
    root.insert("templates".to_owned(), json!({"large": {"chunk": chunk}}));
    for index in 0..=MAX_OUTPUT_BYTES / MAX_STRING_BYTES {
        root.insert(format!("copy-{index}"), json!({"use": "large"}));
    }
    assert!(matches!(
        expand_templates(&Value::Object(root)),
        Err(ConfigSourceError::OutputTooLarge)
    ));
}

#[test]
fn expands_root_vars_including_chained_references_and_removes_vars_block() {
    let source = json!({
        "vars": {
            "addr": "{{host}}:{{edge.port}}",
            "host": "127.0.0.1",
            "edge": {
                "port": 4000,
                "hosts": ["a.example.test", "b.example.test"]
            },
            "copied_hosts": "{{edge.hosts}}",
            "enabled": true,
            "nullable": null,
            "passthrough": "https://$host$request_uri"
        },
        "endpoint": "{{addr}}",
        "port": "{{ edge.port }}",
        "hosts": "{{copied_hosts}}",
        "enabled": "{{enabled}}",
        "nullable": "{{nullable}}",
        "url": "{{passthrough}}"
    });
    let expanded = expand_templates(&source).unwrap();
    assert_eq!(
        expanded,
        json!({
            "endpoint": "127.0.0.1:4000",
            "port": 4000,
            "hosts": ["a.example.test", "b.example.test"],
            "enabled": true,
            "nullable": null,
            "url": "https://$host$request_uri"
        })
    );
    assert!(expanded.get("vars").is_none());
}

#[test]
fn rejects_non_object_vars_var_cycles_and_var_depth_limit() {
    let non_object = expand_templates(&json!({"vars": [1, 2], "item": {}}));
    assert!(matches!(
        non_object,
        Err(ConfigSourceError::Template(message)) if message == "root `vars` must be an object"
    ));

    let cyclic = expand_templates(&json!({
        "vars": {
            "a": "{{b}}",
            "b": "{{a}}"
        },
        "item": "{{a}}"
    }));
    assert!(matches!(cyclic, Err(ConfigSourceError::Template(_))));

    let self_cyclic = expand_templates(&json!({
        "vars": {
            "a": "{{a}}"
        }
    }));
    assert!(matches!(self_cyclic, Err(ConfigSourceError::Template(_))));

    let mut deep_vars = Map::new();
    for index in 0..=MAX_EXPANSION_DEPTH {
        let value = if index == MAX_EXPANSION_DEPTH {
            json!("leaf")
        } else {
            json!(format!("{{{{v-{}}}}}", index + 1))
        };
        deep_vars.insert(format!("v-{index}"), value);
    }
    let deep = expand_templates(&json!({"vars": deep_vars, "out": "{{v-0}}"}));
    assert!(matches!(deep, Err(ConfigSourceError::ExpansionDepth)));
}

#[test]
fn expands_parameterized_templates_with_defaults_overrides_and_shadowing() {
    let source = json!({
        "vars": {
            "domain": "example.test",
            "port": 80,
            "bind": "127.0.0.1"
        },
        "templates": {
            "site": {
                "params": {
                    "port": 3000,
                    "cert": "certbot",
                    "upstream": "{{bind}}:{{port}}"
                },
                "host": "{{sub}}.{{domain}}",
                "port": "{{port}}",
                "cert": "{{cert}}",
                "upstream": "{{upstream}}",
                "nested": {
                    "params": {
                        "kept": true
                    }
                }
            }
        },
        "default_site": {
            "use": { "name": "site", "sub": "blog" }
        },
        "override_site": {
            "use": { "name": "site", "sub": "api", "port": 4000, "cert": "custom" }
        },
        "with_site": {
            "use": {
                "name": "site",
                "with": { "sub": "admin", "port": 5000 },
                "cert": "internal"
            }
        },
        "template_with_site": {
            "use": {
                "template": "site",
                "with": { "sub": "docs", "port": 6000 }
            }
        }
    });

    let expanded = expand_templates(&source).unwrap();
    assert_eq!(
        expanded,
        json!({
            "default_site": {
                "host": "blog.example.test",
                "port": 3000,
                "cert": "certbot",
                "upstream": "127.0.0.1:3000",
                "nested": { "params": { "kept": true } }
            },
            "override_site": {
                "host": "api.example.test",
                "port": 4000,
                "cert": "custom",
                "upstream": "127.0.0.1:4000",
                "nested": { "params": { "kept": true } }
            },
            "with_site": {
                "host": "admin.example.test",
                "port": 5000,
                "cert": "internal",
                "upstream": "127.0.0.1:5000",
                "nested": { "params": { "kept": true } }
            },
            "template_with_site": {
                "host": "docs.example.test",
                "port": 6000,
                "cert": "certbot",
                "upstream": "127.0.0.1:6000",
                "nested": { "params": { "kept": true } }
            }
        })
    );
    assert!(expanded["default_site"].get("params").is_none());
}

#[test]
fn expands_mixed_use_array_and_chained_parameterized_templates() {
    let source = json!({
        "vars": {
            "bind_host": "127.0.0.1"
        },
        "templates": {
            "base": {
                "headers": { "x-base": "1", "x-shared": "base" },
                "tags": ["base"]
            },
            "service_base": {
                "params": { "port": 8080, "scheme": "http" },
                "endpoint": "{{scheme}}://{{bind_host}}:{{port}}",
                "port": "{{port}}",
                "headers": { "x-shared": "service" }
            },
            "tls_service": {
                "params": { "port": 8443 },
                "use": { "name": "service_base", "port": "{{port}}", "scheme": "https" },
                "tls": true,
                "tags": ["tls"]
            }
        },
        "api": {
            "use": [
                "base",
                { "name": "tls_service", "port": 9443 }
            ],
            "headers": { "x-local": "yes" }
        }
    });

    assert_eq!(
        expand_templates(&source).unwrap(),
        json!({
            "api": {
                "endpoint": "https://127.0.0.1:9443",
                "port": 9443,
                "tls": true,
                "tags": ["tls"],
                "headers": {
                    "x-base": "1",
                    "x-shared": "service",
                    "x-local": "yes"
                }
            }
        })
    );
}

#[test]
fn interpolates_typed_values_embedded_strings_dotted_paths_keys_and_escapes() {
    let source = json!({
        "vars": {
            "domain": "oxiroute.dev",
            "port": 4000,
            "hosts": ["a", "b"],
            "enabled": true,
            "ratio": 1.5,
            "header_key": "x-custom",
            "edge": {
                "port": 8443,
                "tls": { "min_version": "1.3" }
            },
            "escaped_var": "\\{{not_interpolated}}"
        },
        "typed_port": "{{port}}",
        "typed_hosts": "{{ hosts }}",
        "typed_object": "{{edge}}",
        "embedded_addr": "127.0.0.1:{{port}}",
        "embedded_wildcard": "*.{{domain}}",
        "embedded_mixed": "ok={{enabled}},ratio={{ratio}},ver={{edge.tls.min_version}}",
        "dotted_port": "{{edge.port}}",
        "escaped_literal": "\\{{literal}}",
        "escaped_and_real": "\\{{literal}} -> {{port}}",
        "single_pass": "{{escaped_var}}",
        "single_pass_embedded": "raw={{escaped_var}}",
        "nginx_passthrough": "https://$host$request_uri",
        "headers": {
            "{{header_key}}": "{{domain}}"
        }
    });

    assert_eq!(
        expand_templates(&source).unwrap(),
        json!({
            "typed_port": 4000,
            "typed_hosts": ["a", "b"],
            "typed_object": {
                "port": 8443,
                "tls": { "min_version": "1.3" }
            },
            "embedded_addr": "127.0.0.1:4000",
            "embedded_wildcard": "*.oxiroute.dev",
            "embedded_mixed": "ok=true,ratio=1.5,ver=1.3",
            "dotted_port": 8443,
            "escaped_literal": "{{literal}}",
            "escaped_and_real": "{{literal}} -> 4000",
            "single_pass": "{{not_interpolated}}",
            "single_pass_embedded": "raw={{not_interpolated}}",
            "nginx_passthrough": "https://$host$request_uri",
            "headers": {
                "x-custom": "oxiroute.dev"
            }
        })
    );
}

#[test]
fn rejects_unknown_vars_unclosed_braces_non_scalar_embedded_and_bad_use_objects() {
    let unknown = expand_templates(&json!({"item": "{{missing}}"}));
    assert!(matches!(
        unknown,
        Err(ConfigSourceError::Template(message)) if message == "unknown template variable `missing`"
    ));

    let invalid_cases = [
        json!({"vars": {"edge": {"port": 80}}, "item": "{{edge.missing}}"}),
        json!({"vars": {"port": 80}, "item": "{{port.sub}}"}),
        json!({"item": "{{unclosed"}),
        json!({"item": "prefix {{unclosed"}),
        json!({"vars": {"a": 1, "b": 2}, "item": "{{a {{b}}"}),
        json!({"item": "{{}}"}),
        json!({"item": "{{   }}"}),
        json!({"vars": {"arr": [1, 2]}, "item": "list={{arr}}"}),
        json!({"vars": {"obj": {"a": 1}}, "item": "obj={{obj}}"}),
        json!({"vars": {"n": null}, "item": "null={{n}}"}),
        json!({"templates": {"t": {"params": 1}}, "item": {"use": "t"}}),
        json!({"templates": {"t": {}}, "item": {"use": {}}}),
        json!({"templates": {"t": {}}, "item": {"use": {"name": 123}}}),
        json!({"templates": {"t": {}}, "item": {"use": {"name": "t", "template": "t"}}}),
        json!({"templates": {"t": {}}, "item": {"use": {"template": "t", "with": 42}}}),
        json!({
            "templates": {
                "a": {"params": {"p": 1}, "use": {"name": "b", "p": "{{p}}"}},
                "b": {"params": {"p": 2}, "use": {"name": "a", "p": "{{p}}"}}
            },
            "item": {"use": {"name": "a", "p": 3}}
        }),
    ];
    for source in invalid_cases {
        assert!(
            matches!(
                expand_templates(&source),
                Err(ConfigSourceError::Template(_))
            ),
            "expected template error for {source}"
        );
    }
}

#[test]
fn enforces_max_substitutions_and_max_string_bytes_bounds() {
    let at_limit: Vec<Value> = (0..MAX_SUBSTITUTIONS).map(|_| json!("{{x}}")).collect();
    let ok = expand_templates(&json!({
        "vars": { "x": "ok" },
        "items": at_limit
    }));
    assert!(ok.is_ok());

    let over_limit: Vec<Value> = (0..=MAX_SUBSTITUTIONS).map(|_| json!("{{x}}")).collect();
    let err = expand_templates(&json!({
        "vars": { "x": "ok" },
        "items": over_limit
    }));
    assert!(matches!(err, Err(ConfigSourceError::SubstitutionLimit)));

    let repeated_uses: Vec<Value> = (0..=MAX_SUBSTITUTIONS)
        .map(|_| json!({"use": "one_sub"}))
        .collect();
    let cached_err = expand_templates(&json!({
        "vars": { "x": "ok" },
        "templates": { "one_sub": { "v": "{{x}}" } },
        "items": repeated_uses
    }));
    assert!(matches!(
        cached_err,
        Err(ConfigSourceError::SubstitutionLimit)
    ));

    let half = "a".repeat((MAX_STRING_BYTES / 2) + 1);
    let too_long = expand_templates(&json!({
        "vars": { "half": half },
        "combined": "{{half}}{{half}}"
    }));
    assert!(matches!(too_long, Err(ConfigSourceError::StringLimit)));
}

#[test]
fn resolves_vars_and_parameterized_templates_identically_across_yaml_and_toml() {
    let yaml_source = br#"
version: 1
vars:
  bind_host: "127.0.0.1"
  edge:
    port: 8443
    workers: 4
    upstream_host: "origin.example.test"
templates:
  http_listener:
    params:
      protocol: http
      port: 8080
    bind:
      type: socket
      address: "{{bind_host}}:{{port}}"
    protocol: "{{protocol}}"
    service: web
listener_workers: "{{edge.workers}}"
listeners:
  - use:
      name: http_listener
      port: "{{edge.port}}"
    name: main-http
upstream_pools:
  - name: origin
    endpoints:
      - type: dns
        host: "{{edge.upstream_host}}"
        port: 443
http_services:
  - name: web
    routes:
      - path:
          kind: segment_prefix
          value: /
        action:
          type: proxy
          upstream_pool: origin
          policy: {}
"#;

    let toml_source = br#"
version = 1
listener_workers = "{{edge.workers}}"

[vars]
bind_host = "127.0.0.1"

[vars.edge]
port = 8443
workers = 4
upstream_host = "origin.example.test"

[templates.http_listener]
protocol = "{{protocol}}"
service = "web"

[templates.http_listener.params]
protocol = "http"
port = 8080

[templates.http_listener.bind]
type = "socket"
address = "{{bind_host}}:{{port}}"

[[listeners]]
name = "main-http"
use = { name = "http_listener", port = "{{edge.port}}" }

[[upstream_pools]]
name = "origin"

[[upstream_pools.endpoints]]
type = "dns"
host = "{{edge.upstream_host}}"
port = 443

[[http_services]]
name = "web"

[[http_services.routes]]
path = { kind = "segment_prefix", value = "/" }
action = { type = "proxy", upstream_pool = "origin", policy = {} }
"#;

    let yaml_resolved =
        resolve_source_with_format(Path::new("oxiroute.yaml"), yaml_source, ConfigFormat::Yaml)
            .expect("YAML templates resolve");
    let toml_resolved =
        resolve_source_with_format(Path::new("oxiroute.toml"), toml_source, ConfigFormat::Toml)
            .expect("TOML templates resolve");

    assert!(yaml_resolved.compositional);
    assert!(toml_resolved.compositional);
    assert_eq!(yaml_resolved.config, toml_resolved.config);
    assert_eq!(yaml_resolved.canonical_kdl, toml_resolved.canonical_kdl);
}
