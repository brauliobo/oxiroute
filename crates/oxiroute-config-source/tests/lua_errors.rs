use std::error::Error as _;

use oxiroute_config::ConfigError;
use oxiroute_config_source::{LuaConfigError, MAX_SOURCE_BYTES, load_lua};

#[test]
fn preserves_lua_runtime_error_text_and_source() {
    let error = load_lua("return os.getenv('SECRET')").unwrap_err();

    assert!(matches!(error, LuaConfigError::Lua(_)));
    assert!(error.to_string().starts_with("Lua configuration failed: "));
    assert!(error.source().is_some());
}

#[test]
fn preserves_canonical_validation_as_its_typed_source() {
    let error = load_lua("return { version = 2, listeners = {} }").unwrap_err();

    assert!(matches!(
        error,
        LuaConfigError::Config(ConfigError::UnsupportedVersion(2))
    ));
    assert_eq!(
        error.to_string(),
        "unsupported configuration version 2; expected version 1"
    );
    assert!(error.source().is_some());
}

#[test]
fn preserves_the_exact_lua_source_bound_error() {
    let error = load_lua(&" ".repeat(MAX_SOURCE_BYTES + 1)).unwrap_err();

    assert!(matches!(error, LuaConfigError::SourceTooLarge));
    assert_eq!(
        error.to_string(),
        format!("configuration exceeds the {MAX_SOURCE_BYTES}-byte source limit")
    );
}
