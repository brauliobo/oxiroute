use std::{
    collections::BTreeMap,
    fmt::{self, Write},
};

use serde_json::{Map, Number, Value};

use crate::{
    ConfigSourceError,
    limits::{BoundedOutput, MAX_STRUCTURAL_DEPTH, validate_value},
};

use super::{is_valid_float, is_valid_int};

const INLINE_ARRAY_MAX_LEN: usize = 96;

pub(super) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    validate_value(value)?;
    let Value::Object(root) = value else {
        return Err(ConfigSourceError::render(
            "OXI",
            "OXI root value must be an object",
        ));
    };

    let mut renderer = Renderer {
        output: BoundedOutput::new(),
    };
    if root.is_empty() {
        renderer
            .output
            .write_char('\n')
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
        return Ok(renderer.output.finish());
    }

    renderer
        .write_object_entries(root, 0, true)
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
    Ok(renderer.output.finish())
}

struct Renderer {
    output: BoundedOutput,
}

impl Renderer {
    fn write_indent(&mut self, spaces: usize) -> fmt::Result {
        for _ in 0..spaces {
            self.output.write_char(' ')?;
        }
        Ok(())
    }

    fn write_object_entries(
        &mut self,
        map: &Map<String, Value>,
        indent: usize,
        is_root: bool,
    ) -> fmt::Result {
        let sorted: BTreeMap<&str, &Value> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
        for (key, val) in sorted {
            self.write_indent(indent)?;
            self.write_key_value(key, val, indent, is_root)?;
        }
        Ok(())
    }

    fn write_key_value(
        &mut self,
        key: &str,
        value: &Value,
        indent: usize,
        is_root: bool,
    ) -> fmt::Result {
        write_key(&mut self.output, key, is_root)?;
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                self.output.write_char(' ')?;
                write_scalar(&mut self.output, value)?;
                self.output.write_char('\n')
            }
            Value::Object(map) if map.is_empty() => self.output.write_str(" {}\n"),
            Value::Object(map) => {
                self.output.write_char('\n')?;
                self.write_object_entries(map, indent + 2, false)
            }
            Value::Array(items) if items.is_empty() => self.output.write_str(" []\n"),
            Value::Array(items) => {
                if let Some(inline) = format_inline_array_value(items) {
                    self.output.write_char(' ')?;
                    self.output.write_str(&inline)?;
                    self.output.write_char('\n')
                } else {
                    self.output.write_char('\n')?;
                    self.write_list_items(items, indent + 2)
                }
            }
        }
    }

    fn write_list_items(&mut self, items: &[Value], indent: usize) -> fmt::Result {
        for item in items {
            self.write_indent(indent)?;
            match item {
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                    self.output.write_str("- ")?;
                    write_scalar(&mut self.output, item)?;
                    self.output.write_char('\n')?;
                }
                Value::Object(map) if map.is_empty() => {
                    self.output.write_str("- {}\n")?;
                }
                Value::Object(map) => {
                    self.write_list_object(map, indent)?;
                }
                Value::Array(inner) if inner.is_empty() => {
                    self.output.write_str("- []\n")?;
                }
                Value::Array(inner) => {
                    if let Some(bracketed) = format_bracketed_inline_array(inner) {
                        self.output.write_str("- ")?;
                        self.output.write_str(&bracketed)?;
                        self.output.write_char('\n')?;
                    } else {
                        self.output.write_str("-\n")?;
                        self.write_list_items(inner, indent + 2)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn write_list_object(&mut self, map: &Map<String, Value>, indent: usize) -> fmt::Result {
        let sorted: BTreeMap<&str, &Value> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let mut iter = sorted.into_iter();
        let Some((first_key, first_val)) = iter.next() else {
            return self.output.write_str("- {}\n");
        };

        if is_inline_field_value(first_val) {
            self.output.write_str("- ")?;
            self.write_key_value(first_key, first_val, indent + 2, false)?;
            for (key, val) in iter {
                self.write_indent(indent + 2)?;
                self.write_key_value(key, val, indent + 2, false)?;
            }
        } else {
            self.output.write_str("-\n")?;
            self.write_indent(indent + 2)?;
            self.write_key_value(first_key, first_val, indent + 2, false)?;
            for (key, val) in iter {
                self.write_indent(indent + 2)?;
                self.write_key_value(key, val, indent + 2, false)?;
            }
        }
        Ok(())
    }
}

fn is_inline_field_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty() || format_inline_array_value(items).is_some(),
    }
}

fn format_inline_array_value(items: &[Value]) -> Option<String> {
    if items.is_empty() {
        return Some("[]".to_owned());
    }
    let mut parts = Vec::with_capacity(items.len());
    let mut total_len = 0usize;
    for item in items {
        let rendered = format_inline_atom(item, 0)?;
        total_len = total_len.saturating_add(rendered.len() + 1);
        if total_len > INLINE_ARRAY_MAX_LEN {
            return None;
        }
        parts.push(rendered);
    }
    if parts.len() == 1 {
        Some(format!("[{}]", parts[0]))
    } else {
        Some(parts.join(" "))
    }
}

fn format_bracketed_inline_array(items: &[Value]) -> Option<String> {
    if items.is_empty() {
        return Some("[]".to_owned());
    }
    let mut parts = Vec::with_capacity(items.len());
    let mut total_len = 2usize;
    for item in items {
        let rendered = format_inline_atom(item, 0)?;
        total_len = total_len.saturating_add(rendered.len() + 1);
        if total_len > INLINE_ARRAY_MAX_LEN {
            return None;
        }
        parts.push(rendered);
    }
    Some(format!("[{}]", parts.join(" ")))
}

fn format_inline_atom(value: &Value, depth: usize) -> Option<String> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return None;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            let mut buf = String::new();
            write_scalar(&mut buf, value).ok()?;
            Some(buf)
        }
        Value::Object(map) if map.is_empty() => Some("{}".to_owned()),
        Value::Array(items) if items.is_empty() => Some("[]".to_owned()),
        _ => None,
    }
}

fn write_key<W: Write>(out: &mut W, key: &str, is_root: bool) -> fmt::Result {
    if is_safe_bare_key(key, is_root) {
        out.write_str(key)
    } else {
        write_double_quoted(out, key)
    }
}

fn write_scalar<W: Write>(out: &mut W, value: &Value) -> fmt::Result {
    match value {
        Value::Null => out.write_str("null"),
        Value::Bool(true) => out.write_str("true"),
        Value::Bool(false) => out.write_str("false"),
        Value::Number(num) => write_number(out, num),
        Value::String(text) => {
            if is_safe_bare_value(text) {
                out.write_str(text)
            } else {
                write_double_quoted(out, text)
            }
        }
        Value::Array(_) | Value::Object(_) => Err(fmt::Error),
    }
}

fn write_number<W: Write>(out: &mut W, number: &Number) -> fmt::Result {
    if number.is_i64() || number.is_u64() {
        return write!(out, "{number}");
    }
    let rendered = number.to_string();
    if rendered.contains('.') {
        return out.write_str(&rendered);
    }
    if let Some(pos) = rendered.find(['e', 'E']) {
        out.write_str(&rendered[..pos])?;
        out.write_str(".0")?;
        out.write_str(&rendered[pos..])
    } else {
        out.write_str(&rendered)?;
        out.write_str(".0")
    }
}

fn write_double_quoted<W: Write>(out: &mut W, text: &str) -> fmt::Result {
    out.write_char('"')?;
    for ch in text.chars() {
        match ch {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\u{0008}' => out.write_str("\\b")?,
            '\u{000C}' => out.write_str("\\f")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{FEFF}') => {
                write!(out, "\\u{:04X}", u32::from(c))?;
            }
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}

fn is_safe_bare_value(text: &str) -> bool {
    if text.is_empty()
        || matches!(
            text,
            "true" | "false" | "on" | "off" | "null" | "none" | "->" | "-" | "{}" | "[]"
        )
        || text.starts_with("lua(")
        || is_valid_int(text)
        || is_valid_float(text)
    {
        return false;
    }
    for ch in text.chars() {
        if ch.is_whitespace()
            || ch.is_control()
            || matches!(
                ch,
                '"' | '\'' | '[' | ']' | '#' | '=' | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
            )
        {
            return false;
        }
    }
    true
}

fn is_safe_bare_key(key: &str, is_root: bool) -> bool {
    if !is_safe_bare_value(key) {
        return false;
    }
    if matches!(key, "lua" | "use" | "route" | "params") {
        return false;
    }
    if is_root
        && (matches!(
            key,
            "include"
                | "site"
                | "template"
                | "pool"
                | "upstream"
                | "cert"
                | "certificate"
                | "tls"
                | "tls_profile"
                | "service"
                | "http_service"
                | "listener"
                | "cache"
                | "cache_store"
                | "forward_proxy"
                | "rtmp"
                | "l4"
        ) || crate::native::is_native_server_key(key))
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        ConfigFormat, ConfigSourceError, MAX_OUTPUT_BYTES, MAX_STRING_BYTES, decode_value,
        render_value,
    };

    #[test]
    fn oxi_renderer_requires_an_object_root() {
        assert!(matches!(
            render_value(ConfigFormat::Oxi, &json!([1, 2])),
            Err(ConfigSourceError::Render { format: "OXI", .. })
        ));
        assert!(matches!(
            render_value(ConfigFormat::Oxi, &json!("scalar")),
            Err(ConfigSourceError::Render { format: "OXI", .. })
        ));
    }

    #[test]
    fn oxi_renderer_enforces_the_shared_output_bound() {
        let oversized = "x".repeat(MAX_STRING_BYTES);
        let value = Value::Object(
            (0..=MAX_OUTPUT_BYTES / MAX_STRING_BYTES)
                .map(|key| (format!("key-{key}"), Value::String(oversized.clone())))
                .collect(),
        );
        assert!(matches!(
            render_value(ConfigFormat::Oxi, &value),
            Err(ConfigSourceError::OutputTooLarge)
        ));
    }

    #[test]
    fn oxi_render_value_round_trips_sorted_generic_trees() {
        let value = json!({
            "z": [true, null],
            "a": {"two": 2, "one": 1},
            "empty_arr": [],
            "empty_obj": {},
            "single": ["only"],
            "include": "not-an-include",
            "site": "literal-site",
            "lua": "not-lua",
            "use": "not-use",
            "reserved_strings": ["on", "off", "true", "false", "null", "none", "123", "1.3", "-0", "->"],
            "escapes": "line1\nline2\t\"quoted\"\\slash\u{0000}nul",
        });

        let rendered = render_value(ConfigFormat::Oxi, &value).expect("OXI render");
        assert!(rendered.ends_with('\n'));
        assert!(!rendered.contains('\r'));
        let decoded = decode_value(ConfigFormat::Oxi, rendered.as_bytes()).expect("OXI decode");
        assert_eq!(decoded, value);
    }
}
