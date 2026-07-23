use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};

use crate::{
    defaults::{
        INSTRUCTION_HOOK_INTERVAL, MAX_LUA_INSTRUCTIONS, MAX_LUA_MEMORY_BYTES, MAX_SOURCE_BYTES,
    },
    model::{Config, ConfigError},
    validation::validate_config,
};

/// Loads a complete immutable configuration snapshot from restricted Lua.
///
/// The restricted environment exposes `null` as the explicit null value. Lua `nil` still means an
/// omitted table field.
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
    lua.globals().set("null", lua.null())?;

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
    let mut config = lua.from_value(value)?;

    validate_config(&mut config)?;
    Ok(config)
}
