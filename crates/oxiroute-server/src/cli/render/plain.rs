use std::fmt::Write as _;

use serde_json::Value;

pub(super) fn render(value: &Value) -> String {
    let mut text = String::new();
    write_value(&mut text, value);
    text
}

pub(super) fn write_value(text: &mut String, value: &Value) {
    match value {
        Value::Null => text.push_str("null\n"),
        Value::Bool(value) => {
            let _ = writeln!(text, "{value}");
        }
        Value::Number(value) => {
            let _ = writeln!(text, "{value}");
        }
        Value::String(value) => {
            let _ = writeln!(text, "{value}");
        }
        Value::Array(values) => values.iter().for_each(|value| write_value(text, value)),
        Value::Object(values) => {
            for (name, value) in values {
                let _ = match value.as_str() {
                    Some(value) => writeln!(text, "{name}={value}"),
                    None => writeln!(text, "{name}={value}"),
                };
            }
        }
    }
}
