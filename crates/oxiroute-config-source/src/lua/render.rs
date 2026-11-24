use std::{collections::BTreeMap, fmt::Write as _};

use serde_json::{Map, Number, Value};

use crate::{ConfigSourceError, limits::BoundedOutput};

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

pub(crate) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    let mut output = BoundedOutput::new();
    output
        .write_str("return ")
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    render_value_at(&mut output, value, 0)?;
    output
        .write_char('\n')
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    Ok(output.finish())
}

fn render_value_at(
    output: &mut BoundedOutput,
    value: &Value,
    indent: usize,
) -> Result<(), ConfigSourceError> {
    match value {
        Value::Null => output
            .write_str("null")
            .map_err(|_| ConfigSourceError::OutputTooLarge),
        Value::Bool(flag) => output
            .write_str(if *flag { "true" } else { "false" })
            .map_err(|_| ConfigSourceError::OutputTooLarge),
        Value::Number(number) => render_number(output, number),
        Value::String(text) => render_string(output, text),
        Value::Array(items) => render_array(output, items, indent),
        Value::Object(map) => render_object(output, map, indent),
    }
}

fn render_number(output: &mut BoundedOutput, number: &Number) -> Result<(), ConfigSourceError> {
    if number.as_i64() == Some(i64::MIN) {
        return output
            .write_str("(-9223372036854775807 - 1)")
            .map_err(|_| ConfigSourceError::OutputTooLarge);
    }
    write!(output, "{number}").map_err(|_| ConfigSourceError::OutputTooLarge)
}

fn render_array(
    output: &mut BoundedOutput,
    items: &[Value],
    indent: usize,
) -> Result<(), ConfigSourceError> {
    if items.is_empty() {
        return output
            .write_str("array({})")
            .map_err(|_| ConfigSourceError::OutputTooLarge);
    }
    output
        .write_str("{\n")
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    for item in items {
        write_indent(output, indent + 1)?;
        render_value_at(output, item, indent + 1)?;
        output
            .write_str(",\n")
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    }
    write_indent(output, indent)?;
    output
        .write_char('}')
        .map_err(|_| ConfigSourceError::OutputTooLarge)
}

fn render_object(
    output: &mut BoundedOutput,
    map: &Map<String, Value>,
    indent: usize,
) -> Result<(), ConfigSourceError> {
    if map.is_empty() {
        return output
            .write_str("{}")
            .map_err(|_| ConfigSourceError::OutputTooLarge);
    }
    let sorted: BTreeMap<&str, &Value> = map
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect();
    output
        .write_str("{\n")
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    for (key, value) in sorted {
        write_indent(output, indent + 1)?;
        render_field_name(output, key)?;
        output
            .write_str(" = ")
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
        render_value_at(output, value, indent + 1)?;
        output
            .write_str(",\n")
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    }
    write_indent(output, indent)?;
    output
        .write_char('}')
        .map_err(|_| ConfigSourceError::OutputTooLarge)
}

fn render_field_name(output: &mut BoundedOutput, name: &str) -> Result<(), ConfigSourceError> {
    let mut chars = name.chars();
    let is_identifier = chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
    if is_identifier && !LUA_KEYWORDS.contains(&name) {
        output
            .write_str(name)
            .map_err(|_| ConfigSourceError::OutputTooLarge)
    } else {
        output
            .write_char('[')
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
        render_string(output, name)?;
        output
            .write_char(']')
            .map_err(|_| ConfigSourceError::OutputTooLarge)
    }
}

fn render_string(output: &mut BoundedOutput, value: &str) -> Result<(), ConfigSourceError> {
    output
        .write_char('"')
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    for character in value.chars() {
        match character {
            '"' => output
                .write_str("\\\"")
                .map_err(|_| ConfigSourceError::OutputTooLarge)?,
            '\\' => output
                .write_str("\\\\")
                .map_err(|_| ConfigSourceError::OutputTooLarge)?,
            '\n' => output
                .write_str("\\n")
                .map_err(|_| ConfigSourceError::OutputTooLarge)?,
            '\r' => output
                .write_str("\\r")
                .map_err(|_| ConfigSourceError::OutputTooLarge)?,
            '\t' => output
                .write_str("\\t")
                .map_err(|_| ConfigSourceError::OutputTooLarge)?,
            character if character.is_control() => {
                write!(output, "\\u{{{:x}}}", u32::from(character))
                    .map_err(|_| ConfigSourceError::OutputTooLarge)?;
            }
            character => output
                .write_char(character)
                .map_err(|_| ConfigSourceError::OutputTooLarge)?,
        }
    }
    output
        .write_char('"')
        .map_err(|_| ConfigSourceError::OutputTooLarge)
}

fn write_indent(output: &mut BoundedOutput, indent: usize) -> Result<(), ConfigSourceError> {
    for _ in 0..indent {
        output
            .write_str("  ")
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    }
    Ok(())
}
