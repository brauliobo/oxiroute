use std::collections::BTreeMap;
use std::ops::Range;

use serde_json::{Map, Number, Value};

use crate::ConfigSourceError;
use crate::limits::{MAX_NODES, MAX_STRUCTURAL_DEPTH, check_output, check_string, validate_value};

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let table = source.parse::<toml::Table>().map_err(|error| {
        if error.message() == "recursion limit" {
            return ConfigSourceError::StructuralDepth;
        }
        let line = line_from_span(source, error.span());
        ConfigSourceError::parse_at("TOML", error.message(), line)
    })?;
    let mut nodes = 0;
    let value = Value::Object(decode_table(table, 0, &mut nodes)?);
    validate_value(&value)?;
    Ok(value)
}

pub(crate) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    let Value::Object(object) = value else {
        return Err(ConfigSourceError::render(
            "TOML",
            "TOML root value must be an object",
        ));
    };
    let table = encode_table(object)?;
    let mut rendered = toml::to_string_pretty(&table)
        .map_err(|error| ConfigSourceError::render("TOML", error.to_string()))?;
    if !rendered.is_empty() && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    check_output(&rendered)?;
    Ok(rendered)
}

fn line_from_span(source: &str, span: Option<Range<usize>>) -> Option<u32> {
    let offset = span?.start.min(source.len());
    let line = source.as_bytes()[..offset]
        .split(|&byte| byte == b'\n')
        .count();
    u32::try_from(line).ok()
}

fn count_node(nodes: &mut usize) -> Result<(), ConfigSourceError> {
    *nodes = nodes.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *nodes > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    Ok(())
}

fn decode_table(
    table: toml::Table,
    depth: usize,
    nodes: &mut usize,
) -> Result<Map<String, Value>, ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    count_node(nodes)?;
    let mut map = Map::with_capacity(table.len());
    for (key, value) in table {
        check_string(&key)?;
        count_node(nodes)?;
        map.insert(key, decode_value(value, depth + 1, nodes)?);
    }
    Ok(map)
}

fn decode_value(
    value: toml::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<Value, ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    match value {
        toml::Value::String(text) => {
            count_node(nodes)?;
            check_string(&text)?;
            Ok(Value::String(text))
        }
        toml::Value::Integer(integer) => {
            count_node(nodes)?;
            Ok(Value::Number(Number::from(integer)))
        }
        toml::Value::Float(float) => {
            count_node(nodes)?;
            let number = Number::from_f64(float).ok_or_else(|| {
                ConfigSourceError::parse(
                    "TOML",
                    "non-finite floating-point numbers are unsupported",
                )
            })?;
            Ok(Value::Number(number))
        }
        toml::Value::Boolean(boolean) => {
            count_node(nodes)?;
            Ok(Value::Bool(boolean))
        }
        toml::Value::Datetime(_) => Err(ConfigSourceError::parse(
            "TOML",
            "TOML datetime values are unsupported",
        )),
        toml::Value::Array(items) => {
            count_node(nodes)?;
            let mut array = Vec::with_capacity(items.len());
            for item in items {
                array.push(decode_value(item, depth + 1, nodes)?);
            }
            Ok(Value::Array(array))
        }
        toml::Value::Table(table) => Ok(Value::Object(decode_table(table, depth, nodes)?)),
    }
}

fn encode_table(object: &Map<String, Value>) -> Result<toml::Table, ConfigSourceError> {
    let sorted: BTreeMap<&str, &Value> = object
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect();
    let mut table = toml::Table::new();
    for (key, value) in sorted {
        table.insert(key.to_owned(), encode_value(value)?);
    }
    Ok(table)
}

fn encode_value(value: &Value) -> Result<toml::Value, ConfigSourceError> {
    match value {
        Value::Null => Err(ConfigSourceError::render(
            "TOML",
            "TOML cannot represent null values",
        )),
        Value::Bool(boolean) => Ok(toml::Value::Boolean(*boolean)),
        Value::String(text) => Ok(toml::Value::String(text.clone())),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Ok(toml::Value::Integer(integer))
            } else if let Some(unsigned) = number.as_u64() {
                Err(ConfigSourceError::render(
                    "TOML",
                    format!("TOML integer out of i64 range: {unsigned}"),
                ))
            } else if let Some(float) = number.as_f64() {
                if !float.is_finite() {
                    return Err(ConfigSourceError::render(
                        "TOML",
                        "non-finite floating-point numbers are unsupported",
                    ));
                }
                Ok(toml::Value::Float(float))
            } else {
                Err(ConfigSourceError::render(
                    "TOML",
                    "number cannot be represented in TOML",
                ))
            }
        }
        Value::Array(items) => items
            .iter()
            .map(encode_value)
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        Value::Object(object) => encode_table(object).map(toml::Value::Table),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        ConfigFormat, ConfigSourceError, MAX_OUTPUT_BYTES, MAX_STRING_BYTES, decode_value,
        render_value,
    };

    #[test]
    fn toml_renderer_requires_an_object_root() {
        for invalid_root in [json!([1, 2]), json!("scalar"), json!(42), json!(true)] {
            assert!(matches!(
                render_value(ConfigFormat::Toml, &invalid_root),
                Err(ConfigSourceError::Render {
                    format: "TOML",
                    ref message,
                    ..
                }) if message == "TOML root value must be an object"
            ));
        }
    }

    #[test]
    fn toml_renderer_rejects_null_values() {
        for invalid in [
            json!({"key": null}),
            json!({"nested": {"inner": null}}),
            json!({"items": [1, null, 3]}),
        ] {
            assert!(matches!(
                render_value(ConfigFormat::Toml, &invalid),
                Err(ConfigSourceError::Render {
                    format: "TOML",
                    ref message,
                    ..
                }) if message == "TOML cannot represent null values"
            ));
        }
    }

    #[test]
    fn toml_renderer_rejects_unsigned_integers_above_i64_max() {
        let too_large = u64::try_from(i64::MAX).unwrap() + 1;
        for value in [json!({"big": too_large}), json!({"big": u64::MAX})] {
            assert!(matches!(
                render_value(ConfigFormat::Toml, &value),
                Err(ConfigSourceError::Render { format: "TOML", .. })
            ));
        }
    }

    #[test]
    fn toml_renderer_enforces_max_output_bytes() {
        let oversized = "x".repeat(MAX_STRING_BYTES);
        let value = Value::Object(
            (0..=MAX_OUTPUT_BYTES / MAX_STRING_BYTES)
                .map(|key| (format!("key-{key}"), Value::String(oversized.clone())))
                .collect(),
        );
        assert!(matches!(
            render_value(ConfigFormat::Toml, &value),
            Err(ConfigSourceError::OutputTooLarge)
        ));
    }

    #[test]
    fn toml_renderer_is_deterministic_with_trailing_lf_and_round_trips() {
        let value = json!({
            "z": [true, false],
            "a": {"two": 2, "one": 1, "ratio": 1.5}
        });
        let first = render_value(ConfigFormat::Toml, &value).unwrap();
        let second = render_value(ConfigFormat::Toml, &value).unwrap();
        assert_eq!(first, second);
        assert!(first.ends_with('\n'));
        assert!(!first.contains("\r\n"));
        assert_eq!(
            decode_value(ConfigFormat::Toml, first.as_bytes()).unwrap(),
            value
        );
    }

    #[test]
    fn toml_renderer_renders_empty_object_without_spurious_newline() {
        let empty = json!({});
        let rendered = render_value(ConfigFormat::Toml, &empty).unwrap();
        assert!(rendered.is_empty());
        assert_eq!(
            decode_value(ConfigFormat::Toml, rendered.as_bytes()).unwrap(),
            empty
        );
    }
}
