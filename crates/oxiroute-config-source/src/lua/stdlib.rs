use std::{
    collections::{BTreeMap, HashSet},
    ffi::c_void,
    fmt::Write as _,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use mlua::{Function, Lua, LuaSerdeExt, LuaString, MultiValue, Table, Value as LuaValue};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::limits::{
    BoundedOutput, MAX_NODES, MAX_STRING_BYTES, MAX_STRUCTURAL_DEPTH, check_output, check_string,
    validate_value,
};

pub(super) fn register(lua: &Lua) -> mlua::Result<()> {
    register_collections(lua)?;
    register_str(lua)?;
    register_codecs(lua)?;
    register_net(lua)?;
    Ok(())
}

fn make_array(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set_metatable(Some(lua.array_metatable()))?;
    Ok(table)
}

fn has_array_metatable(lua: &Lua, table: &Table) -> bool {
    let array_mt_ptr = lua.array_metatable().to_pointer();
    table
        .metatable()
        .is_some_and(|mt| mt.to_pointer() == array_mt_ptr)
}

fn is_array_table(lua: &Lua, table: &Table) -> mlua::Result<bool> {
    if has_array_metatable(lua, table) {
        return Ok(true);
    }
    let mut has_entries = false;
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        has_entries = true;
        match key {
            LuaValue::Integer(index) if index >= 1 => {}
            _ => return Ok(false),
        }
    }
    Ok(has_entries)
}

fn read_list(fn_name: &str, value: &LuaValue) -> mlua::Result<Vec<LuaValue>> {
    let LuaValue::Table(table) = value else {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}() expects a list table argument"
        )));
    };
    let mut entries: Vec<(i64, LuaValue)> = Vec::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, item) = pair?;
        match key {
            LuaValue::Integer(index) if index >= 1 => entries.push((index, item)),
            LuaValue::Integer(index) => {
                return Err(mlua::Error::runtime(format!(
                    "{fn_name}() list index must be >= 1, got {index}"
                )));
            }
            other => {
                return Err(mlua::Error::runtime(format!(
                    "{fn_name}() expects a list table without `{}` keys",
                    other.type_name()
                )));
            }
        }
    }
    entries.sort_by_key(|(index, _)| *index);
    let mut items = Vec::with_capacity(entries.len());
    for (offset, (index, item)) in entries.into_iter().enumerate() {
        let expected = i64::try_from(offset + 1)
            .map_err(|_| mlua::Error::runtime(format!("{fn_name}() list size overflow")))?;
        if index != expected {
            return Err(mlua::Error::runtime(format!(
                "{fn_name}() does not support sparse arrays (expected index {expected}, got {index})"
            )));
        }
        items.push(item);
    }
    Ok(items)
}

fn clone_value(
    lua: &Lua,
    value: &LuaValue,
    depth: usize,
    nodes: &mut usize,
    active: &mut Vec<*const c_void>,
) -> mlua::Result<LuaValue> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(mlua::Error::runtime(
            "clone() exceeded maximum structural depth",
        ));
    }
    *nodes = nodes
        .checked_add(1)
        .ok_or_else(|| mlua::Error::runtime("clone() exceeded maximum node limit"))?;
    if *nodes > MAX_NODES {
        return Err(mlua::Error::runtime("clone() exceeded maximum node limit"));
    }

    let LuaValue::Table(table) = value else {
        return Ok(value.clone());
    };

    let ptr = table.to_pointer();
    if active.contains(&ptr) {
        return Err(mlua::Error::runtime(
            "clone() does not support cyclic tables",
        ));
    }
    active.push(ptr);
    let cloned = clone_table_inner(lua, table, depth, nodes, active);
    active.pop();
    cloned.map(LuaValue::Table)
}

fn clone_table_inner(
    lua: &Lua,
    table: &Table,
    depth: usize,
    nodes: &mut usize,
    active: &mut Vec<*const c_void>,
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    if has_array_metatable(lua, table) {
        out.set_metatable(Some(lua.array_metatable()))?;
    }
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, val) = pair?;
        let cloned_key = clone_value(lua, &key, depth + 1, nodes, active)?;
        let cloned_val = clone_value(lua, &val, depth + 1, nodes, active)?;
        out.raw_set(cloned_key, cloned_val)?;
    }
    Ok(out)
}

fn deep_merge_into(
    lua: &Lua,
    target: &Table,
    source: &Table,
    depth: usize,
    nodes: &mut usize,
    active: &mut Vec<*const c_void>,
) -> mlua::Result<()> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(mlua::Error::runtime(
            "deep_merge() exceeded maximum structural depth",
        ));
    }
    let ptr = source.to_pointer();
    if active.contains(&ptr) {
        return Err(mlua::Error::runtime(
            "deep_merge() does not support cyclic tables",
        ));
    }
    active.push(ptr);
    let result = deep_merge_entries(lua, target, source, depth, nodes, active);
    active.pop();
    result
}

fn deep_merge_entries(
    lua: &Lua,
    target: &Table,
    source: &Table,
    depth: usize,
    nodes: &mut usize,
    active: &mut Vec<*const c_void>,
) -> mlua::Result<()> {
    for pair in source.pairs::<LuaValue, LuaValue>() {
        let (key, val) = pair?;
        *nodes = nodes
            .checked_add(1)
            .ok_or_else(|| mlua::Error::runtime("deep_merge() exceeded maximum node limit"))?;
        if *nodes > MAX_NODES {
            return Err(mlua::Error::runtime(
                "deep_merge() exceeded maximum node limit",
            ));
        }

        let existing: LuaValue = target.raw_get(key.clone())?;
        if let (LuaValue::Table(existing_tbl), LuaValue::Table(src_tbl)) = (&existing, &val)
            && !is_array_table(lua, existing_tbl)?
            && !is_array_table(lua, src_tbl)?
        {
            deep_merge_into(lua, existing_tbl, src_tbl, depth + 1, nodes, active)?;
            continue;
        }

        let cloned_val = clone_value(lua, &val, depth + 1, nodes, active)?;
        target.raw_set(key, cloned_val)?;
    }
    Ok(())
}

#[derive(Eq, Hash, PartialEq)]
enum ScalarKey {
    Nil,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    Str(Vec<u8>),
    LightUserData(usize),
}

fn scalar_key(value: &LuaValue) -> Option<ScalarKey> {
    match value {
        LuaValue::Nil => Some(ScalarKey::Nil),
        LuaValue::Boolean(flag) => Some(ScalarKey::Bool(*flag)),
        LuaValue::Integer(number) => Some(ScalarKey::Int(*number)),
        LuaValue::Number(number) => {
            if !number.is_finite() {
                return None;
            }
            if number.fract() == 0.0
                && *number >= -9_007_199_254_740_992.0
                && *number <= 9_007_199_254_740_992.0
            {
                #[allow(clippy::cast_possible_truncation)]
                let as_int = *number as i64;
                return Some(ScalarKey::Int(as_int));
            }
            let normalized = if *number == 0.0 { 0.0 } else { *number };
            Some(ScalarKey::FloatBits(normalized.to_bits()))
        }
        LuaValue::String(text) => Some(ScalarKey::Str(text.as_bytes().to_vec())),
        LuaValue::LightUserData(data) => Some(ScalarKey::LightUserData(data.0 as usize)),
        _ => None,
    }
}

fn lua_values_equal(
    lua: &Lua,
    left: &LuaValue,
    right: &LuaValue,
    depth: usize,
) -> mlua::Result<bool> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(mlua::Error::runtime(
            "value comparison exceeded maximum structural depth",
        ));
    }
    match (left, right) {
        (LuaValue::Nil, LuaValue::Nil) => Ok(true),
        (LuaValue::Boolean(a), LuaValue::Boolean(b)) => Ok(a == b),
        (LuaValue::Integer(a), LuaValue::Integer(b)) => Ok(a == b),
        (LuaValue::Number(a), LuaValue::Number(b)) => {
            #[allow(clippy::float_cmp)]
            let eq = a == b;
            Ok(eq)
        }
        (LuaValue::Integer(a), LuaValue::Number(b))
        | (LuaValue::Number(b), LuaValue::Integer(a)) => {
            if !b.is_finite() || b.fract() != 0.0 {
                return Ok(false);
            }
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            let exact = (*a as f64) == *b && i128::from(*a) == f64_to_i128_exact(*b);
            Ok(exact)
        }
        (LuaValue::String(a), LuaValue::String(b)) => Ok(a.as_bytes() == b.as_bytes()),
        (LuaValue::LightUserData(a), LuaValue::LightUserData(b)) => Ok(a.0 == b.0),
        (LuaValue::Table(a), LuaValue::Table(b)) => {
            if a.to_pointer() == b.to_pointer() {
                return Ok(true);
            }
            if has_array_metatable(lua, a) != has_array_metatable(lua, b) {
                let a_empty = a.pairs::<LuaValue, LuaValue>().next().is_none();
                let b_empty = b.pairs::<LuaValue, LuaValue>().next().is_none();
                if a_empty && b_empty {
                    return Ok(false);
                }
            }
            let mut a_count = 0usize;
            for pair in a.pairs::<LuaValue, LuaValue>() {
                let (key, val_a) = pair?;
                a_count += 1;
                let val_b: LuaValue = b.raw_get(key)?;
                if val_b.is_nil() || !lua_values_equal(lua, &val_a, &val_b, depth + 1)? {
                    return Ok(false);
                }
            }
            let mut b_count = 0usize;
            for pair in b.pairs::<LuaValue, LuaValue>() {
                let _ = pair?;
                b_count += 1;
            }
            Ok(a_count == b_count)
        }
        _ => Ok(false),
    }
}

fn f64_to_i128_exact(value: f64) -> i128 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as i128
    }
}

fn register_collections(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    let object_fn = lua.create_function(|_, value: LuaValue| {
        let LuaValue::Table(table) = value else {
            return Err(mlua::Error::runtime("object() expects a table argument"));
        };
        table.set_metatable(None)?;
        Ok(table)
    })?;
    globals.set("object", object_fn)?;

    let clone_fn = lua.create_function(|lua, value: LuaValue| {
        let mut nodes = 0usize;
        let mut active = Vec::new();
        clone_value(lua, &value, 0, &mut nodes, &mut active)
    })?;
    globals.set("clone", clone_fn)?;

    let merge_fn = lua.create_function(|lua, args: MultiValue| {
        let out = lua.create_table()?;
        for arg in args {
            let LuaValue::Table(table) = arg else {
                return Err(mlua::Error::runtime("merge() expects table arguments"));
            };
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, val) = pair?;
                out.raw_set(key, val)?;
            }
        }
        Ok(out)
    })?;
    globals.set("merge", merge_fn)?;

    let deep_merge_fn = lua.create_function(|lua, args: MultiValue| {
        let out = lua.create_table()?;
        let mut nodes = 0usize;
        let mut active = Vec::new();
        for arg in args {
            let LuaValue::Table(table) = arg else {
                return Err(mlua::Error::runtime("deep_merge() expects table arguments"));
            };
            deep_merge_into(lua, &out, &table, 0, &mut nodes, &mut active)?;
        }
        Ok(out)
    })?;
    globals.set("deep_merge", deep_merge_fn)?;

    register_key_value_functions(lua)?;
    register_list_transforms(lua)?;
    register_list_queries(lua)?;
    register_list_builders(lua)?;
    Ok(())
}

fn register_key_value_functions(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    let keys_fn = lua.create_function(|lua, value: LuaValue| {
        let LuaValue::Table(table) = value else {
            return Err(mlua::Error::runtime("keys() expects a table argument"));
        };
        let out = make_array(lua)?;
        if is_array_table(lua, &table)? {
            let mut int_keys = Vec::new();
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, _) = pair?;
                let LuaValue::Integer(index) = key else {
                    return Err(mlua::Error::runtime(
                        "keys() expects integer keys for array tables",
                    ));
                };
                int_keys.push(index);
            }
            int_keys.sort_unstable();
            for (idx, key) in int_keys.into_iter().enumerate() {
                out.raw_set(idx + 1, key)?;
            }
        } else {
            let mut str_keys = Vec::new();
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, _) = pair?;
                let LuaValue::String(text) = key else {
                    return Err(mlua::Error::runtime(
                        "keys() expects string keys for object tables",
                    ));
                };
                let key_str = text
                    .to_str()
                    .map_err(|_| mlua::Error::runtime("keys() table key must be valid UTF-8"))?;
                str_keys.push(key_str.to_owned());
            }
            str_keys.sort();
            for (idx, key) in str_keys.into_iter().enumerate() {
                out.raw_set(idx + 1, key)?;
            }
        }
        Ok(out)
    })?;
    globals.set("keys", keys_fn)?;

    let values_fn = lua.create_function(|lua, value: LuaValue| {
        let LuaValue::Table(table) = value else {
            return Err(mlua::Error::runtime("values() expects a table argument"));
        };
        let out = make_array(lua)?;
        if is_array_table(lua, &table)? {
            let mut entries = Vec::new();
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, val) = pair?;
                let LuaValue::Integer(index) = key else {
                    return Err(mlua::Error::runtime(
                        "values() expects integer keys for array tables",
                    ));
                };
                entries.push((index, val));
            }
            entries.sort_by_key(|(index, _)| *index);
            for (idx, (_, val)) in entries.into_iter().enumerate() {
                out.raw_set(idx + 1, val)?;
            }
        } else {
            let mut entries = Vec::new();
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, val) = pair?;
                let LuaValue::String(text) = key else {
                    return Err(mlua::Error::runtime(
                        "values() expects string keys for object tables",
                    ));
                };
                let key_str = text
                    .to_str()
                    .map_err(|_| mlua::Error::runtime("values() table key must be valid UTF-8"))?;
                entries.push((key_str.to_owned(), val));
            }
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (idx, (_, val)) in entries.into_iter().enumerate() {
                out.raw_set(idx + 1, val)?;
            }
        }
        Ok(out)
    })?;
    globals.set("values", values_fn)?;

    Ok(())
}

fn register_list_transforms(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    let map_fn = lua.create_function(|lua, (list, callback): (LuaValue, Function)| {
        let items = read_list("map", &list)?;
        let out = make_array(lua)?;
        for (idx, item) in items.into_iter().enumerate() {
            let mapped: LuaValue = callback.call(item)?;
            out.raw_set(idx + 1, mapped)?;
        }
        Ok(out)
    })?;
    globals.set("map", map_fn)?;

    let filter_fn = lua.create_function(|lua, (list, predicate): (LuaValue, Function)| {
        let items = read_list("filter", &list)?;
        let out = make_array(lua)?;
        let mut out_idx = 0usize;
        for item in items {
            let keep: LuaValue = predicate.call(item.clone())?;
            if !matches!(keep, LuaValue::Nil | LuaValue::Boolean(false)) {
                out_idx += 1;
                out.raw_set(out_idx, item)?;
            }
        }
        Ok(out)
    })?;
    globals.set("filter", filter_fn)?;

    let flat_map_fn = lua.create_function(|lua, (list, callback): (LuaValue, Function)| {
        let items = read_list("flat_map", &list)?;
        let out = make_array(lua)?;
        let mut out_idx = 0usize;
        for item in items {
            let mapped: LuaValue = callback.call(item)?;
            let sub_items = read_list("flat_map", &mapped)?;
            for sub in sub_items {
                out_idx += 1;
                if out_idx > MAX_NODES {
                    return Err(mlua::Error::runtime(
                        "flat_map() exceeded maximum node limit",
                    ));
                }
                out.raw_set(out_idx, sub)?;
            }
        }
        Ok(out)
    })?;
    globals.set("flat_map", flat_map_fn)?;

    let concat_fn = lua.create_function(|lua, args: MultiValue| {
        let out = make_array(lua)?;
        let mut out_idx = 0usize;
        for arg in args {
            let items = read_list("concat", &arg)?;
            for item in items {
                out_idx += 1;
                if out_idx > MAX_NODES {
                    return Err(mlua::Error::runtime("concat() exceeded maximum node limit"));
                }
                out.raw_set(out_idx, item)?;
            }
        }
        Ok(out)
    })?;
    globals.set("concat", concat_fn)?;
    Ok(())
}

fn register_list_queries(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    let unique_fn = lua.create_function(|lua, list: LuaValue| {
        let items = read_list("unique", &list)?;
        let out = make_array(lua)?;
        let mut seen_scalars = HashSet::new();
        let mut seen_complex: Vec<LuaValue> = Vec::new();
        let mut out_idx = 0usize;
        for item in items {
            let is_new = if let Some(key) = scalar_key(&item) {
                seen_scalars.insert(key)
            } else {
                let mut found = false;
                for existing in &seen_complex {
                    if lua_values_equal(lua, existing, &item, 0)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    seen_complex.push(item.clone());
                }
                !found
            };
            if is_new {
                out_idx += 1;
                out.raw_set(out_idx, item)?;
            }
        }
        Ok(out)
    })?;
    globals.set("unique", unique_fn)?;

    let contains_fn = lua.create_function(|lua, (list, target): (LuaValue, LuaValue)| {
        let items = read_list("contains", &list)?;
        for item in &items {
            if lua_values_equal(lua, item, &target, 0)? {
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    globals.set("contains", contains_fn)?;

    let slice_fn =
        lua.create_function(|lua, (list, start, stop): (LuaValue, i64, Option<i64>)| {
            let items = read_list("slice", &list)?;
            let out = make_array(lua)?;
            let len = i64::try_from(items.len())
                .map_err(|_| mlua::Error::runtime("slice() list size overflow"))?;
            if len == 0 {
                return Ok(out);
            }
            let resolve_index = |idx: i64| -> i64 {
                if idx < 0 {
                    len.saturating_add(1).saturating_add(idx)
                } else {
                    idx
                }
            };
            let start_idx = resolve_index(start).max(1);
            let stop_idx = resolve_index(stop.unwrap_or(len)).min(len);
            if start_idx > stop_idx || start_idx > len || stop_idx < 1 {
                return Ok(out);
            }
            let start_zero = usize::try_from(start_idx - 1)
                .map_err(|_| mlua::Error::runtime("slice() start index out of range"))?;
            let stop_exclusive = usize::try_from(stop_idx)
                .map_err(|_| mlua::Error::runtime("slice() stop index out of range"))?;
            for (offset, item) in items[start_zero..stop_exclusive].iter().enumerate() {
                out.raw_set(offset + 1, item.clone())?;
            }
            Ok(out)
        })?;
    globals.set("slice", slice_fn)?;
    Ok(())
}

fn register_list_builders(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    let range_fn = lua.create_function(|lua, (start, stop, step): (i64, i64, Option<i64>)| {
        let step = step.unwrap_or(1);
        if step == 0 {
            return Err(mlua::Error::runtime("range() step must not be zero"));
        }
        let out = make_array(lua)?;
        let mut current = start;
        let mut idx = 0usize;
        loop {
            let in_bounds = if step > 0 {
                current <= stop
            } else {
                current >= stop
            };
            if !in_bounds {
                break;
            }
            idx += 1;
            if idx > MAX_NODES {
                return Err(mlua::Error::runtime("range() exceeded maximum node limit"));
            }
            out.raw_set(idx, current)?;
            let Some(next) = current.checked_add(step) else {
                break;
            };
            current = next;
        }
        Ok(out)
    })?;
    globals.set("range", range_fn)?;

    let group_by_fn = lua.create_function(|lua, (list, key_fn): (LuaValue, Function)| {
        let items = read_list("group_by", &list)?;
        let out = lua.create_table()?;
        for item in items {
            let key: LuaValue = key_fn.call(item.clone())?;
            let LuaValue::String(key_str) = key else {
                return Err(mlua::Error::runtime(
                    "group_by() key function must return a string",
                ));
            };
            let group: Table =
                if let LuaValue::Table(existing) = out.raw_get::<LuaValue>(key_str.clone())? {
                    existing
                } else {
                    let fresh = make_array(lua)?;
                    out.raw_set(key_str, fresh.clone())?;
                    fresh
                };
            let next_idx = group.raw_len() + 1;
            group.raw_set(next_idx, item)?;
        }
        Ok(out)
    })?;
    globals.set("group_by", group_by_fn)?;

    Ok(())
}

fn check_str_limit(value: &str) -> mlua::Result<()> {
    check_string(value).map_err(|error| mlua::Error::runtime(error.to_string()))
}

fn register_str(lua: &Lua) -> mlua::Result<()> {
    let str_tbl = lua.create_table()?;

    let trim_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.trim().to_owned())
    })?;
    str_tbl.set("trim", trim_fn)?;

    let split_fn = lua.create_function(|lua, (text, sep): (String, String)| {
        check_str_limit(&text)?;
        check_str_limit(&sep)?;
        let out = make_array(lua)?;
        if sep.is_empty() {
            for (idx, ch) in text.chars().enumerate() {
                if idx + 1 > MAX_NODES {
                    return Err(mlua::Error::runtime(
                        "str.split() exceeded maximum node limit",
                    ));
                }
                let mut buf = [0u8; 4];
                out.raw_set(idx + 1, ch.encode_utf8(&mut buf) as &str)?;
            }
        } else {
            for (idx, part) in text.split(&sep).enumerate() {
                if idx + 1 > MAX_NODES {
                    return Err(mlua::Error::runtime(
                        "str.split() exceeded maximum node limit",
                    ));
                }
                out.raw_set(idx + 1, part)?;
            }
        }
        Ok(out)
    })?;
    str_tbl.set("split", split_fn)?;

    let starts_with_fn = lua.create_function(|_, (text, prefix): (String, String)| {
        check_str_limit(&text)?;
        check_str_limit(&prefix)?;
        Ok(text.starts_with(&prefix))
    })?;
    str_tbl.set("starts_with", starts_with_fn)?;

    let ends_with_fn = lua.create_function(|_, (text, suffix): (String, String)| {
        check_str_limit(&text)?;
        check_str_limit(&suffix)?;
        Ok(text.ends_with(&suffix))
    })?;
    str_tbl.set("ends_with", ends_with_fn)?;

    let replace_fn = lua.create_function(|_, (text, from, to): (String, String, String)| {
        check_str_limit(&text)?;
        check_str_limit(&from)?;
        check_str_limit(&to)?;
        bounded_replace(&text, &from, &to)
    })?;
    str_tbl.set("replace", replace_fn)?;

    let join_fn = lua.create_function(|_, (list, sep): (LuaValue, Option<String>)| {
        let sep = sep.unwrap_or_default();
        check_str_limit(&sep)?;
        let items = read_list("str.join", &list)?;
        let mut out = String::new();
        for (idx, item) in items.into_iter().enumerate() {
            let LuaValue::String(part) = item else {
                return Err(mlua::Error::runtime(
                    "str.join() expects a list of strings",
                ));
            };
            let part = part
                .to_str()
                .map_err(|_| mlua::Error::runtime("str.join() items must be valid UTF-8"))?;
            if idx > 0 {
                append_bounded_str(&mut out, &sep)?;
            }
            append_bounded_str(&mut out, &part)?;
        }
        Ok(out)
    })?;
    str_tbl.set("join", join_fn)?;

    lua.globals().set("str", str_tbl)?;
    Ok(())
}

fn append_bounded_str(target: &mut String, piece: &str) -> mlua::Result<()> {
    let next_len = target
        .len()
        .checked_add(piece.len())
        .ok_or_else(|| mlua::Error::runtime("string length exceeds maximum allowed size"))?;
    if next_len > MAX_STRING_BYTES {
        return Err(mlua::Error::runtime(format!(
            "string length exceeds maximum allowed size of {MAX_STRING_BYTES} bytes"
        )));
    }
    target.push_str(piece);
    Ok(())
}

fn bounded_replace(text: &str, from: &str, to: &str) -> mlua::Result<String> {
    let mut out = String::new();
    if from.is_empty() {
        append_bounded_str(&mut out, to)?;
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            append_bounded_str(&mut out, ch.encode_utf8(&mut buf))?;
            append_bounded_str(&mut out, to)?;
        }
        return Ok(out);
    }

    let mut last_end = 0usize;
    for (start, matched) in text.match_indices(from) {
        append_bounded_str(&mut out, &text[last_end..start])?;
        append_bounded_str(&mut out, to)?;
        last_end = start + matched.len();
    }
    append_bounded_str(&mut out, &text[last_end..])?;
    Ok(out)
}

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

fn register_codecs(lua: &Lua) -> mlua::Result<()> {
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

fn register_net(lua: &Lua) -> mlua::Result<()> {
    let net_tbl = lua.create_table()?;

    let join_host_port_fn = lua.create_function(|_, (host, port): (String, i64)| {
        check_str_limit(&host)?;
        if host.is_empty() {
            return Err(mlua::Error::runtime(
                "net.join_host_port() host must not be empty",
            ));
        }
        if !(1..=65_535).contains(&port) {
            return Err(mlua::Error::runtime(format!(
                "net.join_host_port() port must be in 1..=65535, got {port}"
            )));
        }
        if host.parse::<Ipv6Addr>().is_ok() {
            return Ok(format!("[{host}]:{port}"));
        }
        if let Some(inner) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            && inner.parse::<Ipv6Addr>().is_ok()
        {
            return Ok(format!("{host}:{port}"));
        }
        if host.contains(':') || host.chars().any(char::is_whitespace) {
            return Err(mlua::Error::runtime(format!(
                "net.join_host_port() invalid host `{host}`"
            )));
        }
        Ok(format!("{host}:{port}"))
    })?;
    net_tbl.set("join_host_port", join_host_port_fn)?;

    let is_ipv4_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.parse::<Ipv4Addr>().is_ok())
    })?;
    net_tbl.set("is_ipv4", is_ipv4_fn)?;

    let is_ipv6_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.parse::<Ipv6Addr>().is_ok())
    })?;
    net_tbl.set("is_ipv6", is_ipv6_fn)?;

    let is_ip_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.parse::<IpAddr>().is_ok())
    })?;
    net_tbl.set("is_ip", is_ip_fn)?;

    let is_cidr_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(oxiroute_config::cidr_contains(&text, IpAddr::V4(Ipv4Addr::UNSPECIFIED)).is_some())
    })?;
    net_tbl.set("is_cidr", is_cidr_fn)?;

    let cidr_contains_fn = lua.create_function(|_, (cidr, ip): (String, String)| {
        check_str_limit(&cidr)?;
        check_str_limit(&ip)?;
        let addr: IpAddr = ip.parse().map_err(|_| {
            mlua::Error::runtime(format!("net.cidr_contains() invalid IP address `{ip}`"))
        })?;
        oxiroute_config::cidr_contains(&cidr, addr).ok_or_else(|| {
            mlua::Error::runtime(format!("net.cidr_contains() invalid CIDR `{cidr}`"))
        })
    })?;
    net_tbl.set("cidr_contains", cidr_contains_fn)?;

    let normalize_host_fn = lua.create_function(|_, host: String| {
        check_str_limit(&host)?;
        let lowered = host.to_ascii_lowercase();
        let normalized = lowered.strip_suffix('.').unwrap_or(&lowered);
        Ok(normalized.to_owned())
    })?;
    net_tbl.set("normalize_host", normalize_host_fn)?;

    lua.globals().set("net", net_tbl)?;
    Ok(())
}
