#![allow(dead_code)]

use oxiroute_config::{ConfigDraft, ConfigError};
use oxiroute_config_source::{ConfigFormat, ConfigSourceError, LuaConfigError, render_config};

pub fn load_lua(source: &str) -> Result<ConfigDraft, ConfigError> {
    match oxiroute_config_source::load_lua(source) {
        Ok(config) => Ok(config.to_draft()),
        Err(LuaConfigError::Config(error)) => Err(error),
        Err(error) => panic!("expected canonical validation error, got {error}"),
    }
}

pub fn render_lua(config: &ConfigDraft) -> Result<String, ConfigError> {
    let validated = config.clone().validate()?;
    match render_config(ConfigFormat::Lua, &validated) {
        Ok(rendered) => Ok(rendered),
        Err(error) => match lua_render_error(error) {
            LuaConfigError::Config(error) => Err(error),
            error => panic!("expected canonical rendering error, got {error}"),
        },
    }
}

pub fn load_lua_error(source: &str) -> LuaConfigError {
    oxiroute_config_source::load_lua(source).expect_err("Lua source must be rejected")
}

pub fn render_lua_error(config: &ConfigDraft) -> LuaConfigError {
    let validated = config
        .clone()
        .validate()
        .expect("valid rendered configuration");
    match render_config(ConfigFormat::Lua, &validated) {
        Err(error) => lua_render_error(error),
        Ok(_) => panic!("Lua rendering must be rejected"),
    }
}

fn lua_render_error(error: ConfigSourceError) -> LuaConfigError {
    let ConfigSourceError::Render {
        source: Some(source),
        ..
    } = error
    else {
        panic!("expected sourced Lua rendering error, got {error}");
    };
    *source
        .downcast::<LuaConfigError>()
        .expect("Lua rendering error source")
}
