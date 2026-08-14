use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};

use oxiroute_config::{ConfigDraft, ValidatedConfig};

use crate::{LuaConfigError, MAX_SOURCE_BYTES};

const MAX_LUA_MEMORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_LUA_INSTRUCTIONS: u32 = 1_000_000;
const INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

/// Loads and validates a complete configuration snapshot from restricted Lua.
///
/// The restricted environment exposes `null` as the explicit null value. Lua `nil` still means an
/// omitted table field.
///
/// # Errors
///
/// Returns an error when evaluation, deserialization, or validation fails.
pub fn load_lua(source: &str) -> Result<ValidatedConfig, LuaConfigError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(LuaConfigError::SourceTooLarge);
    }

    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).map_err(lua_error)?;
    lua.set_memory_limit(lua.used_memory().saturating_add(MAX_LUA_MEMORY_BYTES))
        .map_err(lua_error)?;
    lua.globals().set("null", lua.null()).map_err(lua_error)?;

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
    )
    .map_err(lua_error)?;

    let value = lua
        .load(source)
        .set_name("oxiroute.lua")
        .set_mode(ChunkMode::Text)
        .eval()
        .map_err(lua_error)?;
    let config: ConfigDraft = lua.from_value(value).map_err(lua_error)?;
    config.validate().map_err(LuaConfigError::from)
}

fn lua_error(error: mlua::Error) -> LuaConfigError {
    LuaConfigError::Lua(error)
}
