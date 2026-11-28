use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

mod codecs;
mod dsl;
mod loader;
mod net;
mod render;
mod stdlib;

pub(crate) use render::render;

use mlua::{
    HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value as LuaValue, VmState,
    chunk::ChunkMode,
};
use oxiroute_config::{ConfigDraft, ValidatedConfig};
use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};
use serde_json::{Map, Number, Value};

use crate::{
    ConfigSourceError, LuaConfigError, MAX_NODES, MAX_SOURCE_BYTES, MAX_STRUCTURAL_DEPTH,
    SourceOverrides,
    limits::{check_string, validate_value},
};

const MAX_LUA_MEMORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_LUA_INSTRUCTIONS: u32 = 1_000_000;
const INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

const ALLOWED_GLOBALS: &[&str] = &[
    "assert", "error", "ipairs", "math", "next", "pairs", "pcall", "rawequal", "rawlen", "select",
    "string", "table", "tonumber", "tostring", "type", "utf8", "xpcall",
];

/// Loads and validates a complete configuration snapshot from restricted Lua.
///
/// The restricted environment exposes deterministic Lua 5.4 scripting helpers (`table`, `string`,
/// `math` without RNG, `utf8`, and safe base functions), `null` as the explicit null value, and
/// `array(tbl)` to mark empty tables as arrays. Lua `nil` still means an omitted table field.
///
/// # Errors
///
/// Returns an error when evaluation, deserialization, or validation fails.
pub fn load_lua(source: &str) -> Result<ValidatedConfig, LuaConfigError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(LuaConfigError::SourceTooLarge);
    }

    let deps = loader::LuaDependencies::default();
    let lua = sandboxed_lua(None, None, &deps).map_err(lua_error)?;
    let value: LuaValue = lua
        .load(source)
        .set_name("oxiroute.lua")
        .set_mode(ChunkMode::Text)
        .eval()
        .map_err(lua_error)?;
    let config: ConfigDraft = lua.from_value(value).map_err(lua_error)?;
    config.validate().map_err(LuaConfigError::from)
}

fn sandboxed_lua(
    base_dir: Option<&Path>,
    overrides: Option<&SourceOverrides>,
    dependencies: &loader::LuaDependencies,
) -> mlua::Result<Lua> {
    let stdlib = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8;
    let lua = Lua::new_with(stdlib, LuaOptions::default())?;
    let globals = lua.globals();

    let mut disallowed = Vec::new();
    for pair in globals.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        let keep = match &key {
            LuaValue::String(name) => name
                .to_str()
                .ok()
                .is_some_and(|name| ALLOWED_GLOBALS.contains(&name.as_ref())),
            _ => false,
        };
        if !keep {
            disallowed.push(key);
        }
    }
    for key in disallowed {
        globals.set(key, LuaValue::Nil)?;
    }

    let math: Table = globals.get("math")?;
    math.set("random", LuaValue::Nil)?;
    math.set("randomseed", LuaValue::Nil)?;

    globals.set("null", lua.null())?;
    let array_fn = lua.create_function(|lua, value: LuaValue| {
        let LuaValue::Table(table) = value else {
            return Err(mlua::Error::runtime("array() expects a table argument"));
        };
        table.set_metatable(Some(lua.array_metatable()))?;
        Ok(table)
    })?;
    globals.set("array", array_fn)?;

    stdlib::register(&lua)?;
    dsl::register(&lua)?;
    loader::register(&lua, base_dir, overrides, dependencies)?;

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

    Ok(lua)
}

fn lua_error(error: mlua::Error) -> LuaConfigError {
    LuaConfigError::Lua(error)
}

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    decode_inner(source, None, None, None).map(|(value, _)| value)
}

pub(crate) fn decode_with_base_dir(
    source: &str,
    base_dir: &Path,
    overrides: Option<&SourceOverrides>,
) -> Result<(Value, Vec<PathBuf>), ConfigSourceError> {
    decode_inner(source, None, Some(base_dir), overrides)
}

pub(crate) fn eval_embedded(
    source: &str,
    vars: Option<&Map<String, Value>>,
    base_dir: Option<&Path>,
    overrides: Option<&SourceOverrides>,
) -> Result<(Value, Vec<PathBuf>), ConfigSourceError> {
    decode_inner(source, vars, base_dir, overrides)
}

fn decode_inner(
    source: &str,
    vars: Option<&Map<String, Value>>,
    base_dir: Option<&Path>,
    overrides: Option<&SourceOverrides>,
) -> Result<(Value, Vec<PathBuf>), ConfigSourceError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ConfigSourceError::SourceTooLarge);
    }

    let dependencies = loader::LuaDependencies::default();
    let lua = sandboxed_lua(base_dir, overrides, &dependencies)
        .map_err(lua_error)
        .map_err(ConfigSourceError::Lua)?;
    if let Some(vars) = vars {
        let lua_vars = lua
            .to_value(&Value::Object(vars.clone()))
            .map_err(lua_error)
            .map_err(ConfigSourceError::Lua)?;
        lua.globals()
            .set("vars", lua_vars)
            .map_err(lua_error)
            .map_err(ConfigSourceError::Lua)?;
    }
    let value: LuaValue = lua
        .load(source)
        .set_name("oxiroute.lua")
        .set_mode(ChunkMode::Text)
        .eval()
        .map_err(lua_error)
        .map_err(ConfigSourceError::Lua)?;

    let mut converter = LuaConverter {
        null_value: lua.null(),
        array_mt_ptr: lua.array_metatable().to_pointer(),
        active_tables: Vec::new(),
        nodes: 0,
    };
    let decoded = converter.convert_value(value, 0)?;
    validate_value(&decoded)?;
    Ok((decoded, dependencies.into_paths()))
}

struct LuaConverter {
    null_value: LuaValue,
    array_mt_ptr: *const c_void,
    active_tables: Vec<*const c_void>,
    nodes: usize,
}

impl LuaConverter {
    fn bump_node(&mut self) -> Result<(), ConfigSourceError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(ConfigSourceError::NodeLimit)?;
        if self.nodes > MAX_NODES {
            return Err(ConfigSourceError::NodeLimit);
        }
        Ok(())
    }

    fn convert_value(&mut self, value: LuaValue, depth: usize) -> Result<Value, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        self.bump_node()?;

        if value == self.null_value {
            return Ok(Value::Null);
        }

        match value {
            LuaValue::Nil => Ok(Value::Null),
            LuaValue::Boolean(flag) => Ok(Value::Bool(flag)),
            LuaValue::Integer(number) => Ok(Value::Number(Number::from(number))),
            LuaValue::Number(number) => {
                if !number.is_finite() {
                    return Err(ConfigSourceError::parse(
                        "Lua",
                        "non-finite floating-point number is not supported",
                    ));
                }
                Number::from_f64(number).map(Value::Number).ok_or_else(|| {
                    ConfigSourceError::parse(
                        "Lua",
                        "non-finite floating-point number is not supported",
                    )
                })
            }
            LuaValue::String(text) => {
                let text = text.to_str().map_err(|_| {
                    ConfigSourceError::parse("Lua", "Lua string must be valid UTF-8")
                })?;
                check_string(&text)?;
                Ok(Value::String(text.to_owned()))
            }
            LuaValue::Table(table) => self.convert_table(&table, depth),
            other => Err(ConfigSourceError::parse(
                "Lua",
                format!("unsupported Lua value type `{}`", other.type_name()),
            )),
        }
    }

    fn convert_table(&mut self, table: &Table, depth: usize) -> Result<Value, ConfigSourceError> {
        let ptr = table.to_pointer();
        if self.active_tables.contains(&ptr) {
            return Err(ConfigSourceError::parse(
                "Lua",
                "cyclic Lua table is not supported",
            ));
        }

        self.active_tables.push(ptr);
        let result = self.convert_table_inner(table, depth);
        self.active_tables.pop();
        result
    }

    fn convert_table_inner(
        &mut self,
        table: &Table,
        depth: usize,
    ) -> Result<Value, ConfigSourceError> {
        let is_array_mt = table
            .metatable()
            .is_some_and(|mt| mt.to_pointer() == self.array_mt_ptr);

        let mut int_entries: Vec<(i64, LuaValue)> = Vec::new();
        let mut str_entries: Vec<(String, LuaValue)> = Vec::new();

        for pair in table.pairs::<LuaValue, LuaValue>() {
            let (key, val) = pair.map_err(lua_error).map_err(ConfigSourceError::Lua)?;
            match key {
                LuaValue::Integer(index) => {
                    if index < 1 {
                        return Err(ConfigSourceError::parse(
                            "Lua",
                            format!("Lua array index must be >= 1, got {index}"),
                        ));
                    }
                    int_entries.push((index, val));
                }
                LuaValue::String(key_str) => {
                    let key_str = key_str.to_str().map_err(|_| {
                        ConfigSourceError::parse("Lua", "Lua table key must be valid UTF-8")
                    })?;
                    check_string(&key_str)?;
                    str_entries.push((key_str.to_owned(), val));
                }
                other => {
                    return Err(ConfigSourceError::parse(
                        "Lua",
                        format!(
                            "Lua table keys must be strings or positive integers, got `{}`",
                            other.type_name()
                        ),
                    ));
                }
            }
        }

        if !int_entries.is_empty() && !str_entries.is_empty() {
            return Err(ConfigSourceError::parse(
                "Lua",
                "Lua table cannot mix integer array keys and string object keys",
            ));
        }

        if is_array_mt {
            if !str_entries.is_empty() {
                return Err(ConfigSourceError::parse(
                    "Lua",
                    "Lua array() table cannot contain string keys",
                ));
            }
            return self.convert_array(int_entries, depth);
        }

        if !int_entries.is_empty() {
            return self.convert_array(int_entries, depth);
        }

        str_entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        let mut object = Map::with_capacity(str_entries.len());
        for (key, val) in str_entries {
            self.bump_node()?;
            let converted = self.convert_value(val, depth + 1)?;
            object.insert(key, converted);
        }
        Ok(Value::Object(object))
    }

    fn convert_array(
        &mut self,
        mut entries: Vec<(i64, LuaValue)>,
        depth: usize,
    ) -> Result<Value, ConfigSourceError> {
        entries.sort_by_key(|(index, _)| *index);
        let mut array = Vec::with_capacity(entries.len());
        for (offset, (index, val)) in entries.into_iter().enumerate() {
            let expected = i64::try_from(offset + 1).map_err(|_| ConfigSourceError::NodeLimit)?;
            if index != expected {
                return Err(ConfigSourceError::parse(
                    "Lua",
                    format!(
                        "sparse Lua arrays are not supported (expected index {expected}, got {index})"
                    ),
                ));
            }
            array.push(self.convert_value(val, depth + 1)?);
        }
        Ok(Value::Array(array))
    }
}

/// Deserializes a decoded Lua [`Value`] into a [`ConfigDraft`], treating empty objects `{}`
/// as empty sequences when a sequence is expected.
///
/// This preserves backward compatibility with existing `.lua` files that write `listeners = {}`
/// or `certificates = {}` without `array({})`.
#[allow(dead_code)] // Wired into resolver.rs by the coordinator.
pub(crate) fn deserialize_draft(value: Value) -> Result<ConfigDraft, ConfigSourceError> {
    ConfigDraft::deserialize(LuaValueDeserializer(value))
        .map_err(|error| ConfigSourceError::TypedConfig(error.to_string()))
}

struct LuaValueDeserializer(Value);

impl<'de> Deserializer<'de> for LuaValueDeserializer {
    type Error = serde_json::Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Array(_) => self.deserialize_seq(visitor),
            Value::Object(_) => self.deserialize_map(visitor),
            other => other.deserialize_any(visitor),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_bool(visitor)
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_i8(visitor)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_i16(visitor)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_i32(visitor)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_i64(visitor)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_u8(visitor)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_u16(visitor)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_u32(visitor)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_u64(visitor)
    }

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_f32(visitor)
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_f64(visitor)
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_char(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_str(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_string(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_bytes(visitor)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_byte_buf(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Null => visitor.visit_none(),
            other => visitor.visit_some(Self(other)),
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_unit(visitor)
    }

    fn deserialize_unit_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Array(items) => visitor.visit_seq(LuaSeqAccess {
                iter: items.into_iter(),
            }),
            Value::Object(map) if map.is_empty() => visitor.visit_seq(LuaSeqAccess {
                iter: Vec::new().into_iter(),
            }),
            other => other.deserialize_seq(visitor),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Object(map) => visitor.visit_map(LuaMapAccess {
                iter: map.into_iter(),
                pending: None,
            }),
            other => other.deserialize_map(visitor),
        }
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Array(_) => self.deserialize_seq(visitor),
            Value::Object(_) => self.deserialize_map(visitor),
            other => other.deserialize_struct(name, fields, visitor),
        }
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::String(variant) => visitor.visit_enum(variant.into_deserializer()),
            Value::Object(map) if map.len() == 1 => {
                let mut iter = map.into_iter();
                if let Some((variant, value)) = iter.next() {
                    visitor.visit_enum(LuaEnumAccess { variant, value })
                } else {
                    Value::Object(Map::new()).deserialize_enum(name, variants, visitor)
                }
            }
            other => other.deserialize_enum(name, variants, visitor),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_identifier(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0.deserialize_ignored_any(visitor)
    }
}

struct LuaSeqAccess {
    iter: std::vec::IntoIter<Value>,
}

impl<'de> SeqAccess<'de> for LuaSeqAccess {
    type Error = serde_json::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        self.iter
            .next()
            .map(|value| seed.deserialize(LuaValueDeserializer(value)))
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct LuaMapAccess {
    iter: serde_json::map::IntoIter,
    pending: Option<Value>,
}

impl<'de> MapAccess<'de> for LuaMapAccess {
    type Error = serde_json::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        let Some((key, value)) = self.iter.next() else {
            return Ok(None);
        };
        self.pending = Some(value);
        seed.deserialize(key.into_deserializer()).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let value = self
            .pending
            .take()
            .expect("next_value_seed called without next_key_seed");
        seed.deserialize(LuaValueDeserializer(value))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct LuaEnumAccess {
    variant: String,
    value: Value,
}

impl<'de> de::EnumAccess<'de> for LuaEnumAccess {
    type Error = serde_json::Error;
    type Variant = LuaVariantAccess;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(self.variant.into_deserializer())?;
        Ok((variant, LuaVariantAccess(self.value)))
    }
}

struct LuaVariantAccess(Value);

impl<'de> VariantAccess<'de> for LuaVariantAccess {
    type Error = serde_json::Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        Deserialize::deserialize(LuaValueDeserializer(self.0))
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        seed.deserialize(LuaValueDeserializer(self.0))
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        LuaValueDeserializer(self.0).deserialize_seq(visitor)
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        LuaValueDeserializer(self.0).deserialize_map(visitor)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        ConfigFormat, ConfigSourceError, MAX_OUTPUT_BYTES, MAX_STRING_BYTES, decode_value,
        render_value,
    };

    use super::{decode, deserialize_draft, render};

    #[test]
    fn render_value_round_trips_losslessly_including_empty_containers_and_escapes() {
        let value = json!({
            "z_array": [true, false, null, -42, 0, 9_007_199_254_740_991_i64, i64::MIN, 3.25],
            "a_nested": {
                "empty_array": [],
                "empty_object": {},
                "return": "keyword key",
                "kebab-key": "line1\nline2\t\"quoted\"\\slash\u{0000}nul",
                "utf8": "olá 🌍",
            },
        });

        let rendered = render_value(ConfigFormat::Lua, &value).expect("Lua render");
        assert!(rendered.starts_with("return {\n"));
        assert!(rendered.contains("empty_array = array({})"));
        assert!(rendered.contains("empty_object = {}"));
        assert!(rendered.contains("[\"return\"] = \"keyword key\""));
        assert!(rendered.contains("[\"kebab-key\"] = "));

        let decoded = decode_value(ConfigFormat::Lua, rendered.as_bytes()).expect("Lua decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn render_is_deterministic_and_sorted_by_key() {
        let value = json!({ "z": 1, "m": 2, "a": 3 });
        let rendered = render(&value).expect("Lua render");
        assert_eq!(rendered, "return {\n  a = 3,\n  m = 2,\n  z = 1,\n}\n");
    }

    #[test]
    fn render_enforces_max_output_bytes() {
        let oversized = "x".repeat(MAX_STRING_BYTES);
        let value = Value::Object(
            (0..=MAX_OUTPUT_BYTES / MAX_STRING_BYTES)
                .map(|index| (format!("key-{index}"), Value::String(oversized.clone())))
                .collect(),
        );
        assert!(matches!(
            render_value(ConfigFormat::Lua, &value),
            Err(ConfigSourceError::OutputTooLarge)
        ));
    }

    #[test]
    fn deserialize_draft_accepts_empty_objects_for_sequence_fields() {
        let decoded = decode(
            r"return {
                version = 1,
                listeners = {},
                certificates = {},
                tls_profiles = {},
                http_services = {},
            }",
        )
        .expect("decode Lua");

        // Confirm decode itself preserved empty `{}` as empty JSON objects.
        assert_eq!(decoded["listeners"], json!({}));
        assert_eq!(decoded["certificates"], json!({}));

        let draft = deserialize_draft(decoded).expect("deserialize draft with empty {}");
        let validated = draft.validate().expect("valid config");
        assert_eq!(validated.as_draft().version, 1);
        assert!(validated.as_draft().listeners.is_empty());
        assert!(validated.as_draft().certificates.is_empty());
    }

    #[test]
    fn deserialize_draft_rejects_non_empty_object_for_sequence_field() {
        let decoded = decode(
            r"return {
                version = 1,
                listeners = { not_a_list = true },
            }",
        )
        .expect("decode Lua");

        assert!(matches!(
            deserialize_draft(decoded),
            Err(ConfigSourceError::TypedConfig(_))
        ));
    }
}
