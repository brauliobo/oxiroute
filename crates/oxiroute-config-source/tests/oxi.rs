use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_NODES, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH, decode_value, render_config, resolve_source, resolve_source_with_format,
};
use serde_json::{Value, json};
use tempfile::tempdir;

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture");
    fs::canonicalize(path).expect("canonicalize fixture")
}

#[test]
fn infers_oxi_extension_case_insensitively() {
    for path in ["oxiroute.oxi", "config.OXI", "sub/edge.Oxi"] {
        assert_eq!(ConfigFormat::infer(path).unwrap(), ConfigFormat::Oxi);
    }
}

#[test]
fn round_trips_full_validated_config_through_oxi() {
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
    let rendered = render_config(ConfigFormat::Oxi, &validated).expect("OXI render");
    assert!(rendered.ends_with('\n'));
    assert!(!rendered.contains('\r'));

    let resolved =
        resolve_source(Path::new("oxiroute.oxi"), rendered.as_bytes()).expect("OXI resolve");
    assert_eq!(resolved.format, ConfigFormat::Oxi);
    assert!(!resolved.compositional);
    assert_eq!(resolved.config, validated);

    let explicit =
        resolve_source_with_format(Path::new("config"), rendered.as_bytes(), ConfigFormat::Oxi)
            .expect("explicit OXI resolve");
    assert_eq!(explicit.config, validated);

    let rerendered = render_config(ConfigFormat::Oxi, &resolved.config).expect("second render");
    assert_eq!(rendered, rerendered);
}

#[test]
fn decodes_scalars_quotes_bare_tokens_and_nested_containers() {
    let source = r#"
# Top-level comment
flag_true true # trailing comment
flag_on on
flag_false false
flag_off off
val_null null
val_none none
int_pos 42
int_neg -7
int_zero 0
float_val 3.5
float_sci 1.25e2
leading_zero 007
neg_zero -0
ipv4 127.0.0.1:3000
ipv6 [::]:443
wildcard *.brauliobo.org
path /etc/letsencrypt/live/foo
expr https://$host$request_uri
exact =/health
tmpl {{port}}
hash_inside foo#bar
quoted_bool "on"
quoted_int "123"
quoted_null "null"
quoted_escapes "line1\nline2\t\"quoted\"\\slash\u0041\uD83D\uDE00"
raw_single 'C:\raw\path\d+''ok'''
empty_arr []
empty_obj {}
single_arr [GET]
inline_list brauliobo.org *.brauliobo.org
nested_inline [[1 2] [3 4]]
objects
  - name alpha
    ports 80 443
    meta
      env prod
  -
    nested
      k v
    label beta
matrix
  -
    - 1
    - 2
  -
    - 3
    - 4
"#;

    let decoded = decode_value(ConfigFormat::Oxi, source.as_bytes()).expect("decode OXI");
    assert_eq!(
        decoded,
        json!({
            "flag_true": true,
            "flag_on": true,
            "flag_false": false,
            "flag_off": false,
            "val_null": null,
            "val_none": null,
            "int_pos": 42,
            "int_neg": -7,
            "int_zero": 0,
            "float_val": 3.5,
            "float_sci": 125.0,
            "leading_zero": "007",
            "neg_zero": "-0",
            "ipv4": "127.0.0.1:3000",
            "ipv6": "[::]:443",
            "wildcard": "*.brauliobo.org",
            "path": "/etc/letsencrypt/live/foo",
            "expr": "https://$host$request_uri",
            "exact": "=/health",
            "tmpl": "{{port}}",
            "hash_inside": "foo#bar",
            "quoted_bool": "on",
            "quoted_int": "123",
            "quoted_null": "null",
            "quoted_escapes": "line1\nline2\t\"quoted\"\\slashA😀",
            "raw_single": "C:\\raw\\path\\d+'ok'",
            "empty_arr": [],
            "empty_obj": {},
            "single_arr": ["GET"],
            "inline_list": ["brauliobo.org", "*.brauliobo.org"],
            "nested_inline": [[1, 2], [3, 4]],
            "objects": [
                {"name": "alpha", "ports": [80, 443], "meta": {"env": "prod"}},
                {"nested": {"k": "v"}, "label": "beta"}
            ],
            "matrix": [[1, 2], [3, 4]]
        })
    );
}

#[test]
fn decodes_edge_templates_and_use_shorthands() {
    let source = r#"
version 1

vars
  domain brauliobo.org
  api_port 5000

edge
  http 0.0.0.0:80 [::]:80 -> https
  https 0.0.0.0:443 [::]:443

template app
  params port=3000 cert=certbot tls=on label="web app"
  proxy 127.0.0.1:{{port}}

template hardened
  headers
    x-frame-options DENY

site main
  use hardened
  use app port=4000 cert=certbot
  host brauliobo.org *.brauliobo.org

site multi_use
  use base extra
  use third
"#;

    let decoded = decode_value(ConfigFormat::Oxi, source.as_bytes()).expect("decode stanzas");
    assert_eq!(
        decoded,
        json!({
            "version": 1,
            "vars": { "domain": "brauliobo.org", "api_port": 5000 },
            "edge": {
                "http": ["0.0.0.0:80", "[::]:80"],
                "redirect_https": true,
                "https": ["0.0.0.0:443", "[::]:443"]
            },
            "templates": {
                "app": {
                    "params": {
                        "port": 3000,
                        "cert": "certbot",
                        "tls": true,
                        "label": "web app"
                    },
                    "proxy": "127.0.0.1:{{port}}"
                },
                "hardened": {
                    "headers": { "x-frame-options": "DENY" }
                }
            },
            "sites": {
                "main": {
                    "use": [
                        "hardened",
                        { "name": "app", "port": 4000, "cert": "certbot" }
                    ],
                    "host": ["brauliobo.org", "*.brauliobo.org"]
                },
                "multi_use": {
                    "use": ["base", "extra", "third"]
                }
            }
        })
    );
}

#[test]
fn decodes_site_route_shorthands_and_action_merges() {
    let source = r"
site main
  route =/health -> 200 ok
    respond
      content_type text/plain
  route /empty -> 204
  route /api -> 127.0.0.1:5000
    methods [GET POST]
    max_request_body_bytes 1048576
    policy
      timeout_ms 5000
  route /pool -> proxy @backend
  route /assets -> static /var/www/assets
    static
      index index.html
  route /old -> redirect 301 https://brauliobo.org/new
  route /moved -> redirect https://brauliobo.org/latest
";

    let decoded = decode_value(ConfigFormat::Oxi, source.as_bytes()).expect("decode routes");
    assert_eq!(
        decoded["sites"]["main"]["routes"],
        json!([
            {
                "path": "=/health",
                "respond": { "status": 200, "body": "ok", "content_type": "text/plain" }
            },
            { "path": "/empty", "respond": { "status": 204 } },
            {
                "path": "/api",
                "methods": ["GET", "POST"],
                "max_request_body_bytes": 1_048_576,
                "policy": { "timeout_ms": 5000 },
                "proxy": "127.0.0.1:5000"
            },
            { "path": "/pool", "proxy": "@backend" },
            { "path": "/assets", "static": { "root": "/var/www/assets", "index": "index.html" } },
            { "path": "/old", "redirect": { "status": 301, "to": "https://brauliobo.org/new" } },
            { "path": "/moved", "redirect": { "status": 308, "to": "https://brauliobo.org/latest" } }
        ])
    );
}

#[test]
fn resolves_named_collection_stanzas_and_templates_into_validated_config() {
    let source = r#"
version 1

template base_pool
  algorithm round_robin

cert main-cert
  dns_names [app.example.test]
  source
    type files
    certificate_chain_path /etc/oxiroute/chain.pem
    private_key_path /etc/oxiroute/key.pem

tls main-tls
  certificates [main-cert]
  default_certificate main-cert
  min_version "1.3"
  alpn h2 http/1.1

pool backend
  use base_pool
  endpoints
    - type socket
      address 127.0.0.1:3000

cache mem
  type memory

service web
  routes
    - path
        kind segment_prefix
        value /
      action
        type proxy
        upstream_pool backend
        policy {}

listener https-in
  bind
    type socket
    address 127.0.0.1:8443
  protocol http
  service web
  tls_profile main-tls
"#;

    let resolved = resolve_source(Path::new("edge.oxi"), source.as_bytes()).expect("resolve OXI");
    assert_eq!(resolved.format, ConfigFormat::Oxi);
    assert!(resolved.compositional);

    let draft = resolved.config.as_draft();
    assert_eq!(draft.certificates.len(), 1);
    assert_eq!(draft.certificates[0].name, "main-cert");
    assert_eq!(draft.tls_profiles.len(), 1);
    assert_eq!(draft.tls_profiles[0].name, "main-tls");
    assert_eq!(draft.upstream_pools.len(), 1);
    assert_eq!(draft.upstream_pools[0].name, "backend");
    assert_eq!(draft.cache_stores.len(), 1);
    assert_eq!(draft.http_services.len(), 1);
    assert_eq!(draft.listeners.len(), 1);
    assert_eq!(draft.listeners[0].name, "https-in");
}

#[test]
fn decodes_all_named_collections_and_native_server_shorthands() {
    let source = r"
upstream p1
  algorithm round_robin
certificate c1
  dns_names [a.test]
tls_profile t1
  default_certificate c1
http_service s1
  routes []
cache_store cs1
  type memory
forward_proxy fp1
  access_policy {}
rtmp r1
  applications []
l4 tcp1
  upstream_pool p1

nginx_server /etc/nginx/a.conf
  x_accel_controls_absent on
nginx_server /etc/nginx/b.conf
haproxy_server /etc/haproxy/a.cfg /etc/haproxy/b.cfg
  node_ip 10.0.0.1
squid_server /etc/squid/squid.conf
  externalize_cache true
apache_server /etc/apache2/apache2.conf
varnish_server /etc/varnish/default.vcl
  arguments [-p default_ttl=120]
";

    let decoded = decode_value(ConfigFormat::Oxi, source.as_bytes()).expect("decode shorthands");
    assert_eq!(decoded["upstream_pools"][0]["name"], "p1");
    assert_eq!(decoded["certificates"][0]["name"], "c1");
    assert_eq!(decoded["tls_profiles"][0]["name"], "t1");
    assert_eq!(decoded["http_services"][0]["name"], "s1");
    assert_eq!(decoded["cache_stores"][0]["name"], "cs1");
    assert_eq!(decoded["forward_proxy_services"][0]["name"], "fp1");
    assert_eq!(decoded["rtmp_services"][0]["name"], "r1");
    assert_eq!(decoded["l4_services"][0]["name"], "tcp1");

    assert_eq!(
        decoded["nginx_server"],
        json!([
            {"path": "/etc/nginx/a.conf", "x_accel_controls_absent": true},
            {"path": "/etc/nginx/b.conf"}
        ])
    );
    assert_eq!(
        decoded["haproxy_server"],
        json!({
            "paths": ["/etc/haproxy/a.cfg", "/etc/haproxy/b.cfg"],
            "node_ip": "10.0.0.1"
        })
    );
    assert_eq!(
        decoded["squid_server"],
        json!({"path": "/etc/squid/squid.conf", "externalize_cache": true})
    );
    assert_eq!(
        decoded["apache_server"],
        json!({"path": "/etc/apache2/apache2.conf"})
    );
    assert_eq!(
        decoded["varnish_server"],
        json!({"path": "/etc/varnish/default.vcl", "arguments": ["-p", "default_ttl=120"]})
    );
}

#[test]
fn evaluates_embedded_lua_blocks_and_inline_expressions_with_vars_dsl_and_net() {
    let source = r#"
version 1

vars
  host 127.0.0.1
  base_port 4000
  workers 3

listener_workers lua(vars.workers + 1)

lua
  local pools = {}
  for i = 1, 2 do
    pools[i] = oxi.upstream("app-" .. i, net.join_host_port(vars.host, vars.base_port + i))
  end
  return {
    upstream_pools = pools,
    http_services = {
      oxi.http_service("web", {
        oxi.route("/v1", oxi.proxy("app-1"), { host = "api.example.test" }),
      }),
    },
  }

listeners
  lua
    return {
      oxi.listener("http", net.join_host_port(vars.host, 8080), "web"),
    }
"#;

    let resolved = resolve_source(Path::new("embedded.oxi"), source.as_bytes())
        .expect("resolve embedded Lua in OXI");
    let draft = resolved.config.as_draft();
    assert_eq!(draft.listener_workers, Some(4));
    assert_eq!(draft.upstream_pools.len(), 2);
    assert_eq!(draft.upstream_pools[0].name, "app-1");
    assert_eq!(draft.upstream_pools[1].name, "app-2");
    assert_eq!(draft.http_services.len(), 1);
    assert_eq!(draft.listeners.len(), 1);
    assert_eq!(draft.listeners[0].name, "http");
}

#[test]
fn supports_key_equals_lua_block_and_enforces_lua_sandbox() {
    let key_eq_source = r#"
vars
  prefix app

generated = lua
  return {
    first = vars.prefix .. "-1",
    second = net.join_host_port("::1", 8443),
  }
"#;
    let decoded =
        decode_value(ConfigFormat::Oxi, key_eq_source.as_bytes()).expect("key = lua decode");
    assert_eq!(
        decoded["generated"],
        json!({ "first": "app-1", "second": "[::1]:8443" })
    );

    let sandbox_escape = r#"
secret lua(os.getenv("HOME"))
"#;
    let err = decode_value(ConfigFormat::Oxi, sandbox_escape.as_bytes())
        .expect_err("os.getenv must be rejected by Lua sandbox");
    assert!(matches!(err, ConfigSourceError::Lua(_)));
}

#[test]
fn includes_oxi_yaml_toml_json_and_lua_and_tracks_dependencies() {
    let dir = tempdir().expect("create tempdir");
    let root_path = dir.path().join("oxiroute.oxi");

    let nested_oxi = write_file(
        &dir.path().join("sub/pools.oxi"),
        r"
pool api-pool
  algorithm round_robin
  endpoints
    - type socket
      address 127.0.0.1:4001
",
    );
    let sub_entry = write_file(
        &dir.path().join("sub/entry.oxi"),
        r"
include pools.oxi
",
    );
    let cert_yaml = write_file(
        &dir.path().join("certs.yaml"),
        r"
certificates:
  - name: edge-cert
    dns_names:
      - api.example.test
    source:
      type: files
      certificate_chain_path: /etc/oxiroute/chain.pem
      private_key_path: /etc/oxiroute/key.pem
",
    );
    let tls_toml = write_file(
        &dir.path().join("tls.toml"),
        r#"
[[tls_profiles]]
name = "edge-tls"
certificates = ["edge-cert"]
default_certificate = "edge-cert"
min_version = "1.3"
alpn = ["h2", "http/1.1"]
"#,
    );
    let cache_json = write_file(
        &dir.path().join("cache.json"),
        r#"{ "cache_stores": [{ "name": "mem", "type": "memory" }] }"#,
    );
    let lua_nested_data = write_file(&dir.path().join("lua_mods/host.txt"), "api.example.test");
    let routes_lua = write_file(
        &dir.path().join("lua_mods/services.lua"),
        r#"
local host = string.gsub(read_text("host.txt"), "%s+", "")
return {
  http_services = {
    oxi.http_service("web", {
      oxi.route("/v1", oxi.proxy("api-pool"), { host = host }),
    }),
  },
}
"#,
    );

    let root_source = r"
version 1
include sub/entry.oxi
include certs.yaml
include tls.toml
include cache.json
include lua_mods/services.lua

listener https-in
  bind
    type socket
    address 127.0.0.1:8443
  protocol http
  service web
  tls_profile edge-tls
";

    let resolved = resolve_source(&root_path, root_source.as_bytes()).expect("resolve includes");
    assert_eq!(resolved.format, ConfigFormat::Oxi);
    assert!(resolved.compositional);

    let draft = resolved.config.as_draft();
    assert_eq!(draft.upstream_pools.len(), 1);
    assert_eq!(draft.certificates.len(), 1);
    assert_eq!(draft.tls_profiles.len(), 1);
    assert_eq!(draft.cache_stores.len(), 1);
    assert_eq!(draft.http_services.len(), 1);
    assert_eq!(draft.listeners.len(), 1);

    for expected_dep in [
        &sub_entry,
        &nested_oxi,
        &cert_yaml,
        &tls_toml,
        &cache_json,
        &routes_lua,
        &lua_nested_data,
    ] {
        assert!(
            resolved.dependencies.contains(expected_dep),
            "missing dependency: {}",
            expected_dep.display()
        );
    }
}

#[test]
fn enforces_include_sandbox_security_and_cycle_detection() {
    let dir = tempdir().expect("create tempdir");
    let root_path = dir.path().join("oxiroute.oxi");

    // Memory-only decode_value disables include.
    let mem_err = decode_value(ConfigFormat::Oxi, b"include other.oxi\n")
        .expect_err("memory-only include must fail");
    assert!(matches!(
        mem_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(1),
            ..
        }
    ));

    // Parent directory traversal rejected.
    let escape_err = resolve_source(&root_path, b"version 1\ninclude ../secret.oxi\n")
        .expect_err(".. traversal must fail");
    assert!(matches!(
        escape_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(2),
            ..
        }
    ));

    // Absolute path rejected.
    let abs_err = resolve_source(&root_path, b"version 1\ninclude /etc/passwd\n")
        .expect_err("absolute include must fail");
    assert!(matches!(
        abs_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(2),
            ..
        }
    ));

    // Include cycle rejected.
    write_file(&dir.path().join("a.oxi"), "include b.oxi\n");
    write_file(&dir.path().join("b.oxi"), "include a.oxi\n");
    let cycle_err = resolve_source(&root_path, b"version 1\ninclude a.oxi\n")
        .expect_err("include cycle must fail");
    assert!(matches!(
        cycle_err,
        ConfigSourceError::Parse { format: "OXI", .. }
    ));

    // Symlink rejected on Unix.
    #[cfg(unix)]
    {
        let outside = tempdir().expect("outside tempdir");
        let outside_file = write_file(&outside.path().join("secret.oxi"), "version 1\n");
        let link_path = dir.path().join("linked.oxi");
        std::os::unix::fs::symlink(&outside_file, &link_path).expect("create symlink");
        let sym_err = resolve_source(&root_path, b"version 1\ninclude linked.oxi\n")
            .expect_err("symlink include must fail");
        assert!(matches!(
            sym_err,
            ConfigSourceError::Parse {
                format: "OXI",
                line: Some(2),
                ..
            }
        ));
    }
}

#[test]
fn reports_line_numbers_for_tabs_duplicates_and_syntax_errors() {
    let tab_src = "version 1\nedge\n\thttp 0.0.0.0:80\n";
    let tab_err = decode_value(ConfigFormat::Oxi, tab_src.as_bytes()).expect_err("tab indent");
    assert!(matches!(
        tab_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(3),
            ..
        }
    ));

    let dup_key = "version 1\nfoo 1\nfoo 2\n";
    let dup_err = decode_value(ConfigFormat::Oxi, dup_key.as_bytes()).expect_err("duplicate key");
    assert!(matches!(
        dup_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(3),
            ..
        }
    ));

    let dup_site = "site web\n  host a.test\nsite web\n  host b.test\n";
    let dup_site_err =
        decode_value(ConfigFormat::Oxi, dup_site.as_bytes()).expect_err("duplicate site");
    assert!(matches!(
        dup_site_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(3),
            ..
        }
    ));

    let unclosed = "a 1\nb \"unterminated\n";
    let unclosed_err =
        decode_value(ConfigFormat::Oxi, unclosed.as_bytes()).expect_err("unterminated quote");
    assert!(matches!(
        unclosed_err,
        ConfigSourceError::Parse {
            format: "OXI",
            line: Some(2),
            ..
        }
    ));
}

#[test]
fn enforces_source_depth_string_and_node_bounds() {
    // Source size bound.
    let huge_src = vec![b' '; MAX_SOURCE_BYTES + 1];
    assert!(matches!(
        decode_value(ConfigFormat::Oxi, &huge_src),
        Err(ConfigSourceError::SourceTooLarge)
    ));

    // String length bound.
    let mut long_str = String::from("k \"");
    long_str.push_str(&"a".repeat(MAX_STRING_BYTES + 1));
    long_str.push_str("\"\n");
    assert!(matches!(
        decode_value(ConfigFormat::Oxi, long_str.as_bytes()),
        Err(ConfigSourceError::StringLimit)
    ));

    // Structural depth bound.
    let mut deep = String::new();
    for level in 0..=(MAX_STRUCTURAL_DEPTH + 2) {
        let indent = "  ".repeat(level);
        writeln!(deep, "{indent}k{level}").expect("write depth");
    }
    let last_indent = "  ".repeat(MAX_STRUCTURAL_DEPTH + 3);
    writeln!(deep, "{last_indent}leaf 1").expect("write leaf");
    assert!(matches!(
        decode_value(ConfigFormat::Oxi, deep.as_bytes()),
        Err(ConfigSourceError::StructuralDepth)
    ));

    // Node count bound (compact 1-char items inside an inline array stay well under MAX_SOURCE_BYTES).
    let mut many = String::from("a [");
    for _ in 0..=MAX_NODES {
        many.push_str("1 ");
    }
    many.push_str("]\n");
    assert!(many.len() < MAX_SOURCE_BYTES);
    assert!(matches!(
        decode_value(ConfigFormat::Oxi, many.as_bytes()),
        Err(ConfigSourceError::NodeLimit)
    ));

    // Empty document decodes cleanly to `{}`.
    assert_eq!(
        decode_value(ConfigFormat::Oxi, b"# only a comment\n\n").unwrap(),
        Value::Object(serde_json::Map::new())
    );
}
