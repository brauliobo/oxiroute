use std::error::Error as _;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, render_config, resolve_source_with_format,
};
use serde_json::json;

fn oversized_lua_config() -> ValidatedConfig {
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "cache_stores": [{
            "type": "memory",
            "name": "x".repeat(1024 * 1024)
        }]
    }))
    .expect("oversized output draft")
    .validate()
    .expect("oversized output remains canonically valid")
}

#[test]
fn oversized_lua_output_uses_the_render_error_path_with_its_source() {
    let error = render_config(ConfigFormat::Lua, &oversized_lua_config()).unwrap_err();

    assert!(matches!(
        error,
        ConfigSourceError::Render {
            format: "Lua",
            ref message,
            ..
        } if message.contains("source limit")
    ));
    assert!(error.source().is_some());
}

#[test]
fn malformed_lua_loading_remains_a_lua_parse_error() {
    let error = resolve_source_with_format(
        std::path::Path::new("broken.lua"),
        b"return { version = 1, listeners = {",
        ConfigFormat::Lua,
    )
    .unwrap_err();

    assert!(matches!(error, ConfigSourceError::Lua(_)));
}
