use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::ConfigSourceError;

/// Maximum accepted source size.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum rendered source or expanded JSON size.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum nesting depth in a decoded or rendered value.
pub const MAX_STRUCTURAL_DEPTH: usize = 128;
/// Maximum number of values and object keys in a value tree.
pub const MAX_NODES: usize = 100_000;
/// Maximum UTF-8 byte length of a string value, object key, or source identifier.
pub const MAX_STRING_BYTES: usize = 256 * 1024;
/// Maximum recursive template inheritance depth.
pub const MAX_EXPANSION_DEPTH: usize = 64;

pub(crate) fn source_text(source: &[u8]) -> Result<&str, ConfigSourceError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ConfigSourceError::SourceTooLarge);
    }
    Ok(std::str::from_utf8(source)?)
}

pub(crate) fn check_string(value: &str) -> Result<(), ConfigSourceError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(ConfigSourceError::StringLimit);
    }
    Ok(())
}

pub(crate) fn validate_value(value: &Value) -> Result<(), ConfigSourceError> {
    let mut nodes = 0;
    validate_at(value, 0, &mut nodes)
}

fn validate_at(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    *nodes = nodes.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *nodes > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    match value {
        Value::String(value) => check_string(value),
        Value::Array(values) => {
            for value in values {
                validate_at(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                check_string(key)?;
                *nodes = nodes.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
                if *nodes > MAX_NODES {
                    return Err(ConfigSourceError::NodeLimit);
                }
                validate_at(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

pub(crate) fn check_output(output: &str) -> Result<(), ConfigSourceError> {
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(ConfigSourceError::OutputTooLarge);
    }
    Ok(())
}

pub(crate) fn render_sorted_json(value: &Value) -> Result<String, ConfigSourceError> {
    let sorted = sort_value(value);
    let mut output = serde_json::to_string_pretty(&sorted)
        .map_err(|error| ConfigSourceError::render("HOCON", error.to_string()))?;
    output.push('\n');
    Ok(output)
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| (key.clone(), sort_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(values) => Value::Array(values.iter().map(sort_value).collect()),
        value => value.clone(),
    }
}
