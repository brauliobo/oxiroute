#[path = "../lua_support/mod.rs"]
mod lua_support;

use lua_support::{load_lua, render_lua};
use serde_json::{Value, json};

fn proxy_config(compression: &str) -> String {
    format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  upstream_pools = {{
    {{ name = "origin", endpoints = {{ {{ type = "socket", address = "127.0.0.1:3000" }} }} }},
  }},
  http_services = {{
    {{ name = "web", routes = {{
      {{
        path = {{ kind = "segment_prefix", value = "/" }},
        action = {{ type = "proxy", upstream_pool = "origin", policy = {{
          cache = {{ store = "memory"{compression} }}
        }} }},
      }},
    }} }},
  }},
}}"#
    )
}

fn cache(source: &str) -> Value {
    let config = load_lua(source).expect("valid compression config");
    let value = serde_json::to_value(config).expect("serialized config");
    value["http_services"][0]["routes"][0]["action"]["policy"]["cache"].clone()
}

fn rejection(compression: &str) -> String {
    oxiroute_config_source::load_lua(&proxy_config(compression))
        .expect_err("compression must be rejected")
        .to_string()
}

#[test]
fn absent_compression_stays_out_of_the_canonical_form() {
    assert!(cache(&proxy_config(""))["compression"].is_null());
}

#[test]
fn an_empty_block_applies_the_measured_defaults() {
    let compression = &cache(&proxy_config(", compression = {}"))["compression"];
    assert_eq!(compression["codings"], json!(["zstd", "br", "gzip"]));
    assert_eq!(compression["min_length_bytes"], 256);
    assert_eq!(compression["gzip_level"], 6);
    assert_eq!(compression["brotli_level"], 5);
    assert_eq!(compression["zstd_level"], 7);
    assert_eq!(compression["precompress"], json!([]));
    assert!(
        compression["content_types"]
            .as_array()
            .expect("content types")
            .contains(&json!("text/html"))
    );
}

#[test]
fn configured_values_round_trip_through_the_canonical_lua_form() {
    let source = proxy_config(
        r#", compression = {
            codings = { "gzip", "zstd" },
            precompress = { "gzip" },
            content_types = { "text/html", "application/json" },
            min_length_bytes = 1024,
            gzip_level = 9,
            brotli_level = 11,
            zstd_level = -3,
        }"#,
    );
    let config = load_lua(&source).expect("valid compression config");
    let rendered = render_lua(&config).expect("rendered config");
    let reloaded = load_lua(&rendered).expect("rendered config reloads");
    assert_eq!(config, reloaded);
    let compression = &cache(&source)["compression"];
    assert_eq!(compression["codings"], json!(["gzip", "zstd"]));
    assert_eq!(compression["precompress"], json!(["gzip"]));
    assert_eq!(compression["zstd_level"], -3);
}

#[test]
fn invalid_compression_blocks_are_rejected_with_their_field() {
    let cases = [
        (
            ", compression = { codings = { \"gzip\", \"gzip\" } }",
            "compression.codings",
        ),
        (
            ", compression = { precompress = { \"br\" }, codings = { \"gzip\" } }",
            "compression.precompress",
        ),
        (
            ", compression = { content_types = { \"html\" } }",
            "compression.content_types",
        ),
        (
            ", compression = { gzip_level = 0 }",
            "compression.gzip_level",
        ),
        (
            ", compression = { gzip_level = 13 }",
            "compression.gzip_level",
        ),
        (
            ", compression = { brotli_level = 12 }",
            "compression.brotli_level",
        ),
        (
            ", compression = { zstd_level = 20 }",
            "compression.zstd_level",
        ),
        (
            ", compression = { zstd_level = -8 }",
            "compression.zstd_level",
        ),
        (", compression = { codings = { \"deflate\" } }", "deflate"),
        (", compression = { speed = 1 }", "speed"),
    ];
    for (compression, field) in cases {
        let message = rejection(compression);
        assert!(message.contains(field), "{compression}: {message}");
    }
}
