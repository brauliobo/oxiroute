use std::{collections::BTreeMap, fmt::Write as _};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use mlua::{Lua, LuaSerdeExt, LuaString, Value as LuaValue};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::stdlib::check_str_limit;
use crate::limits::{BoundedOutput, MAX_STRING_BYTES, check_output, validate_value};

fn lua_to_validated_json(lua: &Lua, value: LuaValue) -> mlua::Result<Value> {
    let json_val: Value = lua.from_value(value)?;
    validate_value(&json_val).map_err(|error| mlua::Error::runtime(error.to_string()))?;
    Ok(json_val)
}

fn sort_json_value(value: &Value) -> Value {
    match value {
        Value::Object(entries) => {
            let sorted: BTreeMap<String, Value> = entries
                .iter()
                .map(|(key, val)| (key.clone(), sort_json_value(val)))
                .collect();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json_value).collect()),
        scalar => scalar.clone(),
    }
}

pub(super) fn register(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // json
    let json_tbl = lua.create_table()?;
    let json_encode = lua.create_function(|lua, value: LuaValue| {
        let json_val = lua_to_validated_json(lua, value)?;
        let sorted = sort_json_value(&json_val);
        let mut output = BoundedOutput::new();
        serde_json::to_writer(&mut output, &sorted).map_err(|error| {
            if output.exceeded() {
                mlua::Error::runtime("json.encode() output exceeds maximum allowed size")
            } else {
                mlua::Error::runtime(format!("json.encode() failed: {error}"))
            }
        })?;
        let rendered = output.finish();
        check_output(&rendered).map_err(|error| mlua::Error::runtime(error.to_string()))?;
        Ok(rendered)
    })?;
    json_tbl.set("encode", json_encode)?;

    let json_decode = lua.create_function(|lua, text: String| {
        check_str_limit(&text)?;
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|error| mlua::Error::runtime(format!("json.decode() failed: {error}")))?;
        validate_value(&parsed).map_err(|error| mlua::Error::runtime(error.to_string()))?;
        lua.to_value(&parsed)
    })?;
    json_tbl.set("decode", json_decode)?;
    globals.set("json", json_tbl)?;

    // yaml
    let yaml_tbl = lua.create_table()?;
    let yaml_decode = lua.create_function(|lua, text: String| {
        check_str_limit(&text)?;
        let parsed = crate::yaml::decode(&text)
            .map_err(|error| mlua::Error::runtime(format!("yaml.decode() failed: {error}")))?;
        lua.to_value(&parsed)
    })?;
    yaml_tbl.set("decode", yaml_decode)?;

    let yaml_encode = lua.create_function(|lua, value: LuaValue| {
        let json_val = lua_to_validated_json(lua, value)?;
        crate::yaml::render(&json_val)
            .map_err(|error| mlua::Error::runtime(format!("yaml.encode() failed: {error}")))
    })?;
    yaml_tbl.set("encode", yaml_encode)?;
    globals.set("yaml", yaml_tbl)?;

    // toml
    let toml_tbl = lua.create_table()?;
    let toml_decode = lua.create_function(|lua, text: String| {
        check_str_limit(&text)?;
        let parsed = crate::toml::decode(&text)
            .map_err(|error| mlua::Error::runtime(format!("toml.decode() failed: {error}")))?;
        lua.to_value(&parsed)
    })?;
    toml_tbl.set("decode", toml_decode)?;

    let toml_encode = lua.create_function(|lua, value: LuaValue| {
        let json_val = lua_to_validated_json(lua, value)?;
        crate::toml::render(&json_val)
            .map_err(|error| mlua::Error::runtime(format!("toml.encode() failed: {error}")))
    })?;
    toml_tbl.set("encode", toml_encode)?;
    globals.set("toml", toml_tbl)?;

    register_encoding_and_hash(lua)?;
    Ok(())
}

fn register_encoding_and_hash(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // base64
    let base64_tbl = lua.create_table()?;
    let b64_encode = lua.create_function(|_, text: LuaString| {
        let bytes = text.as_bytes();
        if bytes.len() > MAX_STRING_BYTES {
            return Err(mlua::Error::runtime(format!(
                "base64.encode() input exceeds maximum allowed size of {MAX_STRING_BYTES} bytes"
            )));
        }
        let encoded = STANDARD.encode(&bytes);
        check_str_limit(&encoded)?;
        Ok(encoded)
    })?;
    base64_tbl.set("encode", b64_encode)?;

    let b64_decode = lua.create_function(|lua, text: String| {
        check_str_limit(&text)?;
        let decoded = STANDARD
            .decode(text.as_bytes())
            .map_err(|error| mlua::Error::runtime(format!("base64.decode() failed: {error}")))?;
        if decoded.len() > MAX_STRING_BYTES {
            return Err(mlua::Error::runtime(format!(
                "base64.decode() output exceeds maximum allowed size of {MAX_STRING_BYTES} bytes"
            )));
        }
        lua.create_string(&decoded)
    })?;
    base64_tbl.set("decode", b64_decode)?;

    let b64_encode_url = lua.create_function(|_, text: LuaString| {
        let bytes = text.as_bytes();
        if bytes.len() > MAX_STRING_BYTES {
            return Err(mlua::Error::runtime(format!(
                "base64.encode_url() input exceeds maximum allowed size of {MAX_STRING_BYTES} bytes"
            )));
        }
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);
        check_str_limit(&encoded)?;
        Ok(encoded)
    })?;
    base64_tbl.set("encode_url", b64_encode_url)?;

    let b64_decode_url = lua.create_function(|lua, text: String| {
        check_str_limit(&text)?;
        let decoded = if text.contains('=') {
            URL_SAFE.decode(text.as_bytes())
        } else {
            URL_SAFE_NO_PAD.decode(text.as_bytes())
        }
        .map_err(|error| mlua::Error::runtime(format!("base64.decode_url() failed: {error}")))?;
        if decoded.len() > MAX_STRING_BYTES {
            return Err(mlua::Error::runtime(format!(
                "base64.decode_url() output exceeds maximum allowed size of {MAX_STRING_BYTES} bytes"
            )));
        }
        lua.create_string(&decoded)
    })?;
    base64_tbl.set("decode_url", b64_decode_url)?;
    globals.set("base64", base64_tbl)?;

    // hash
    let hash_tbl = lua.create_table()?;
    let sha256_fn = lua.create_function(|_, text: LuaString| {
        let bytes = text.as_bytes();
        if bytes.len() > MAX_STRING_BYTES {
            return Err(mlua::Error::runtime(format!(
                "hash.sha256() input exceeds maximum allowed size of {MAX_STRING_BYTES} bytes"
            )));
        }
        let digest = Sha256::digest(&bytes);
        let mut hex = String::with_capacity(64);
        for byte in digest {
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(hex)
    })?;
    hash_tbl.set("sha256", sha256_fn)?;
    globals.set("hash", hash_tbl)?;

    Ok(())
}
