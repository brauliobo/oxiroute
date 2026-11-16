use oxiroute_config::ConfigDraft;
use serde_json::{Value, json};

fn draft(compression: &Value) -> ConfigDraft {
    serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "cache_stores": [{"type": "memory", "name": "memory"}],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{"type": "socket", "address": "127.0.0.1:3000"}]
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": {"kind": "segment_prefix", "value": "/"},
                "action": {
                    "type": "proxy",
                    "upstream_pool": "origin",
                    "policy": {"cache": {"store": "memory", "compression": compression}}
                }
            }]
        }]
    }))
    .expect("draft decodes")
}

fn rejection(compression: &Value) -> String {
    draft(compression)
        .validate()
        .expect_err("compression must be rejected")
        .to_string()
}

#[test]
fn defaults_validate() {
    draft(&json!({})).validate().expect("default compression");
}

#[test]
fn empty_lists_are_rejected() {
    assert!(rejection(&json!({"codings": []})).contains("compression.codings"));
    assert!(rejection(&json!({"content_types": []})).contains("compression.content_types"));
}

#[test]
fn duplicates_and_unoffered_precompression_are_rejected() {
    assert!(rejection(&json!({"codings": ["gzip", "gzip"]})).contains("compression.codings"));
    assert!(rejection(&json!({"precompress": ["br", "br"]})).contains("compression.precompress"));
    assert!(
        rejection(&json!({"codings": ["gzip"], "precompress": ["zstd"]}))
            .contains("compression.precompress")
    );
    assert!(
        rejection(&json!({"content_types": ["text/html", "TEXT/HTML"]}))
            .contains("compression.content_types")
    );
}

#[test]
fn levels_outside_the_codec_ranges_are_rejected() {
    for (field, level) in [
        ("gzip_level", 0),
        ("gzip_level", 13),
        ("brotli_level", 12),
        ("zstd_level", 20),
        ("zstd_level", -8),
    ] {
        let message = rejection(&json!({ field: level }));
        assert!(
            message.contains(&format!("compression.{field}")),
            "{message}"
        );
    }
}

#[test]
fn level_extremes_are_accepted() {
    for compression in [
        json!({"gzip_level": 1, "brotli_level": 0, "zstd_level": -7}),
        json!({"gzip_level": 12, "brotli_level": 11, "zstd_level": 19}),
    ] {
        draft(&compression).validate().expect("level extremes");
    }
}
