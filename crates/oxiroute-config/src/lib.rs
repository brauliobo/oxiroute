use std::{
    collections::HashSet,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};
use serde::Deserialize;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_LUA_MEMORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_LUA_INSTRUCTIONS: u32 = 1_000_000;
const INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub management: Option<Management>,
    pub listeners: Vec<Listener>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Management {
    pub bind: SocketAddr,
    #[serde(default)]
    pub ui_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Listener {
    pub name: String,
    pub bind: SocketAddr,
    pub protocol: Protocol,
    #[serde(default)]
    pub upstream: Option<SocketAddr>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Rtmp,
    Tcp,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Lua configuration failed: {0}")]
    Lua(#[from] mlua::Error),
    #[error("configuration exceeds the {MAX_SOURCE_BYTES}-byte source limit")]
    SourceTooLarge,
    #[error("unsupported configuration version {0}; expected version 1")]
    UnsupportedVersion(u32),
    #[error("listener name cannot be empty")]
    EmptyListenerName,
    #[error("duplicate listener name `{0}`")]
    DuplicateListenerName(String),
    #[error("duplicate listener bind `{0}`")]
    DuplicateListenerBind(SocketAddr),
    #[error("listener `{listener}` has an invalid zero port in `{field}`")]
    ZeroPort {
        listener: String,
        field: &'static str,
    },
    #[error("{protocol:?} listener `{listener}` requires an upstream")]
    MissingUpstream {
        listener: String,
        protocol: Protocol,
    },
    #[error("RTMP listener `{0}` must not declare an upstream")]
    UnexpectedRtmpUpstream(String),
    #[error("management listener must use loopback, got `{0}`")]
    ManagementMustUseLoopback(SocketAddr),
}

/// Loads a complete immutable configuration snapshot from restricted Lua.
///
/// # Errors
///
/// Returns an error when evaluation, deserialization, or validation fails.
pub fn load_lua(source: &str) -> Result<Config, ConfigError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ConfigError::SourceTooLarge);
    }

    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default())?;
    lua.set_memory_limit(lua.used_memory().saturating_add(MAX_LUA_MEMORY_BYTES))?;

    let instructions = Arc::new(AtomicU32::new(MAX_LUA_INSTRUCTIONS));
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(INSTRUCTION_HOOK_INTERVAL),
        move |_lua, _debug| {
            if instructions.fetch_sub(INSTRUCTION_HOOK_INTERVAL, Ordering::Relaxed)
                <= INSTRUCTION_HOOK_INTERVAL
            {
                return Err(mlua::Error::runtime("Lua instruction limit exceeded"));
            }

            Ok(VmState::Continue)
        },
    )?;

    let value = lua
        .load(source)
        .set_name("oxiroute.lua")
        .set_mode(ChunkMode::Text)
        .eval()?;
    let config = lua.from_value(value)?;

    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    if config.version != 1 {
        return Err(ConfigError::UnsupportedVersion(config.version));
    }

    if let Some(management) = &config.management {
        if !management.bind.ip().is_loopback() {
            return Err(ConfigError::ManagementMustUseLoopback(management.bind));
        }
        if management.bind.port() == 0 {
            return Err(ConfigError::ZeroPort {
                listener: "management".into(),
                field: "bind",
            });
        }
    }

    let mut names = HashSet::with_capacity(config.listeners.len());
    let mut binds = HashSet::with_capacity(config.listeners.len());

    for listener in &config.listeners {
        if listener.name.trim().is_empty() {
            return Err(ConfigError::EmptyListenerName);
        }
        if !names.insert(listener.name.as_str()) {
            return Err(ConfigError::DuplicateListenerName(listener.name.clone()));
        }
        if !binds.insert(listener.bind) {
            return Err(ConfigError::DuplicateListenerBind(listener.bind));
        }
        if listener.bind.port() == 0 {
            return Err(ConfigError::ZeroPort {
                listener: listener.name.clone(),
                field: "bind",
            });
        }
        match (listener.protocol, listener.upstream) {
            (Protocol::Http | Protocol::Tcp, None) => {
                return Err(ConfigError::MissingUpstream {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                });
            }
            (Protocol::Rtmp, Some(_)) => {
                return Err(ConfigError::UnexpectedRtmpUpstream(listener.name.clone()));
            }
            _ => {}
        }
        if listener
            .upstream
            .is_some_and(|upstream| upstream.port() == 0)
        {
            return Err(ConfigError::ZeroPort {
                listener: listener.name.clone(),
                field: "upstream",
            });
        }
    }

    Ok(())
}
