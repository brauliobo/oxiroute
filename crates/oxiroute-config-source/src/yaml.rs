use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use saphyr::{LoadableYamlNode, MarkedYaml, Marker, Scalar, ScanError, YamlData};
use serde_json::{Map, Number, Value};

use crate::ConfigSourceError;
use crate::limits::{BoundedOutput, MAX_NODES, MAX_STRUCTURAL_DEPTH, check_string, validate_value};

const SOURCE_INDEX_CHUNK_CHARS: usize = 256;

struct SourceIndex<'a> {
    source: &'a str,
    checkpoints: Vec<usize>,
}

impl<'a> SourceIndex<'a> {
    fn new(source: &'a str) -> Self {
        if source.is_ascii() {
            return Self {
                source,
                checkpoints: Vec::new(),
            };
        }
        let mut checkpoints = Vec::new();
        for (char_index, (byte_offset, _)) in source.char_indices().enumerate() {
            if char_index % SOURCE_INDEX_CHUNK_CHARS == 0 {
                checkpoints.push(byte_offset);
            }
        }
        Self {
            source,
            checkpoints,
        }
    }

    fn slice_from(&self, char_index: usize) -> Option<&'a str> {
        if self.checkpoints.is_empty() {
            return self.source.get(char_index..);
        }
        let chunk = char_index / SOURCE_INDEX_CHUNK_CHARS;
        let remainder = char_index % SOURCE_INDEX_CHUNK_CHARS;
        let start_byte = *self.checkpoints.get(chunk)?;
        let tail = self.source.get(start_byte..)?;
        if remainder == 0 {
            return Some(tail);
        }
        tail.char_indices()
            .nth(remainder)
            .and_then(|(offset, _)| tail.get(offset..))
    }
}

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let documents = MarkedYaml::load_from_str(source).map_err(|error| parse_scan_error(&error))?;
    if documents.is_empty() {
        return Err(ConfigSourceError::parse("YAML", "YAML document is empty"));
    }
    if documents.len() > 1 {
        return Err(ConfigSourceError::parse_at(
            "YAML",
            "multi-document YAML streams are not supported",
            marker_line(documents[1].span.start),
        ));
    }

    let source_index = SourceIndex::new(source);
    if matches!(documents[0].data, YamlData::Value(Scalar::Null))
        && !is_explicit_null_token(&source_index, documents[0].span.start)
    {
        return Err(ConfigSourceError::parse_at(
            "YAML",
            "YAML document is empty",
            marker_line(documents[0].span.start),
        ));
    }
    let mut nodes = 0usize;
    let value = convert_node(&documents[0], 0, &mut nodes, &source_index)?;
    validate_value(&value)?;
    Ok(value)
}

fn parse_scan_error(error: &ScanError) -> ConfigSourceError {
    ConfigSourceError::parse_at("YAML", error.info(), marker_line(*error.marker()))
}

fn is_explicit_null_token(source_index: &SourceIndex<'_>, start: Marker) -> bool {
    source_index
        .slice_from(start.index())
        .is_some_and(|slice| slice.starts_with(['~', 'n', 'N']))
}

fn marker_line(marker: Marker) -> Option<u32> {
    let line = marker.line();
    if line == 0 {
        None
    } else {
        u32::try_from(line).ok()
    }
}

fn count_node(nodes: &mut usize) -> Result<(), ConfigSourceError> {
    *nodes = nodes.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *nodes > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    Ok(())
}

fn reject_node_properties(
    node: &MarkedYaml<'_>,
    source_index: &SourceIndex<'_>,
) -> Result<(), ConfigSourceError> {
    let line = marker_line(node.span.start);
    if let Some(slice) = source_index.slice_from(node.span.start.index()) {
        if slice.starts_with('&') {
            return Err(ConfigSourceError::parse_at(
                "YAML",
                "YAML anchors are not supported",
                line,
            ));
        }
        if slice.starts_with('*') {
            return Err(ConfigSourceError::parse_at(
                "YAML",
                "YAML aliases are not supported",
                line,
            ));
        }
        if slice.starts_with('!') {
            return Err(ConfigSourceError::parse_at(
                "YAML",
                "YAML tags are not supported",
                line,
            ));
        }
    }
    Ok(())
}

fn convert_node(
    node: &MarkedYaml<'_>,
    depth: usize,
    nodes: &mut usize,
    source_index: &SourceIndex<'_>,
) -> Result<Value, ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    count_node(nodes)?;
    reject_node_properties(node, source_index)?;

    let line = marker_line(node.span.start);
    match &node.data {
        YamlData::Value(scalar) => convert_scalar(scalar, line),
        YamlData::Sequence(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(convert_node(item, depth + 1, nodes, source_index)?);
            }
            Ok(Value::Array(values))
        }
        YamlData::Mapping(entries) => {
            let mut map = Map::with_capacity(entries.len());
            for (key_node, value_node) in entries {
                reject_node_properties(key_node, source_index)?;
                let key_line = marker_line(key_node.span.start);
                let YamlData::Value(Scalar::String(key)) = &key_node.data else {
                    return Err(ConfigSourceError::parse_at(
                        "YAML",
                        "mapping keys must be strings",
                        key_line,
                    ));
                };
                let key_str = key.as_ref();
                check_string(key_str)?;
                count_node(nodes)?;
                if map.contains_key(key_str) {
                    return Err(ConfigSourceError::parse_at(
                        "YAML",
                        format!("duplicate mapping key `{key_str}`"),
                        key_line,
                    ));
                }
                let value = convert_node(value_node, depth + 1, nodes, source_index)?;
                map.insert(key_str.to_owned(), value);
            }
            Ok(Value::Object(map))
        }
        YamlData::Tagged(..) => Err(ConfigSourceError::parse_at(
            "YAML",
            "YAML tags are not supported",
            line,
        )),
        YamlData::Alias(_) => Err(ConfigSourceError::parse_at(
            "YAML",
            "YAML aliases are not supported",
            line,
        )),
        YamlData::Representation(..) => Err(ConfigSourceError::parse_at(
            "YAML",
            "unparsed YAML representation is not supported",
            line,
        )),
        YamlData::BadValue => Err(ConfigSourceError::parse_at(
            "YAML",
            "invalid or empty YAML document",
            line,
        )),
    }
}

fn convert_scalar(scalar: &Scalar<'_>, line: Option<u32>) -> Result<Value, ConfigSourceError> {
    match scalar {
        Scalar::Null => Ok(Value::Null),
        Scalar::Boolean(boolean) => Ok(Value::Bool(*boolean)),
        Scalar::Integer(integer) => Ok(Value::Number(Number::from(*integer))),
        Scalar::FloatingPoint(float) => {
            let value = float.into_inner();
            if !value.is_finite() {
                return Err(ConfigSourceError::parse_at(
                    "YAML",
                    "non-finite floating-point values are not supported",
                    line,
                ));
            }
            let number = Number::from_f64(value).ok_or_else(|| {
                ConfigSourceError::parse_at(
                    "YAML",
                    "non-finite floating-point values are not supported",
                    line,
                )
            })?;
            Ok(Value::Number(number))
        }
        Scalar::String(text) => {
            check_string(text.as_ref())?;
            Ok(Value::String(text.as_ref().to_owned()))
        }
    }
}

pub(crate) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    let mut output = BoundedOutput::new();
    let result = match value {
        Value::Object(map) if map.is_empty() => output.write_str("{}\n"),
        Value::Object(map) => render_object(&mut output, map, 0, false),
        Value::Array(items) if items.is_empty() => output.write_str("[]\n"),
        Value::Array(items) => render_array(&mut output, items, 0),
        scalar => render_scalar(&mut output, scalar).and_then(|()| output.write_char('\n')),
    };
    result.map_err(|_| {
        if output.exceeded() {
            ConfigSourceError::OutputTooLarge
        } else {
            ConfigSourceError::render("YAML", "failed to format YAML output")
        }
    })?;
    Ok(output.finish())
}

fn write_indent(output: &mut BoundedOutput, indent: usize) -> fmt::Result {
    for _ in 0..indent {
        output.write_char(' ')?;
    }
    Ok(())
}

fn render_object(
    output: &mut BoundedOutput,
    map: &Map<String, Value>,
    indent: usize,
    inline_first_key: bool,
) -> fmt::Result {
    let sorted: BTreeMap<&str, &Value> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
    for (index, (key, value)) in sorted.into_iter().enumerate() {
        if index > 0 || !inline_first_key {
            write_indent(output, indent)?;
        }
        render_string(output, key)?;
        output.write_char(':')?;
        match value {
            Value::Object(child) if child.is_empty() => {
                output.write_str(" {}\n")?;
            }
            Value::Object(child) => {
                output.write_char('\n')?;
                render_object(output, child, indent + 2, false)?;
            }
            Value::Array(child) if child.is_empty() => {
                output.write_str(" []\n")?;
            }
            Value::Array(child) => {
                output.write_char('\n')?;
                render_array(output, child, indent + 2)?;
            }
            scalar => {
                output.write_char(' ')?;
                render_scalar(output, scalar)?;
                output.write_char('\n')?;
            }
        }
    }
    Ok(())
}

fn render_array(output: &mut BoundedOutput, items: &[Value], indent: usize) -> fmt::Result {
    for item in items {
        write_indent(output, indent)?;
        match item {
            Value::Object(map) if map.is_empty() => {
                output.write_str("- {}\n")?;
            }
            Value::Object(map) => {
                output.write_str("- ")?;
                render_object(output, map, indent + 2, true)?;
            }
            Value::Array(child) if child.is_empty() => {
                output.write_str("- []\n")?;
            }
            Value::Array(child) => {
                output.write_str("-\n")?;
                render_array(output, child, indent + 2)?;
            }
            scalar => {
                output.write_str("- ")?;
                render_scalar(output, scalar)?;
                output.write_char('\n')?;
            }
        }
    }
    Ok(())
}

fn render_scalar(output: &mut BoundedOutput, value: &Value) -> fmt::Result {
    match value {
        Value::Null => output.write_str("null"),
        Value::Bool(true) => output.write_str("true"),
        Value::Bool(false) => output.write_str("false"),
        Value::Number(number) => write!(output, "{number}"),
        Value::String(text) => render_string(output, text),
        Value::Array(_) | Value::Object(_) => unreachable!("containers are handled separately"),
    }
}

fn render_string(output: &mut BoundedOutput, value: &str) -> fmt::Result {
    if is_safe_plain_scalar(value) {
        output.write_str(value)
    } else {
        write_double_quoted(output, value)
    }
}

fn is_safe_plain_scalar(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let last = chars.next_back().unwrap_or(first);

    if first.is_whitespace() || last.is_whitespace() || last == ':' {
        return false;
    }

    if matches!(
        first,
        '-' | '?'
            | ':'
            | ','
            | '['
            | ']'
            | '{'
            | '}'
            | '#'
            | '&'
            | '*'
            | '!'
            | '|'
            | '>'
            | '\''
            | '"'
            | '%'
            | '@'
            | '`'
    ) {
        return false;
    }

    if value.starts_with("---") || value.starts_with("...") {
        return false;
    }

    if is_yaml_reserved_word(value) {
        return false;
    }

    if !matches!(
        Scalar::parse_from_cow(Cow::Borrowed(value)),
        Scalar::String(_)
    ) {
        return false;
    }

    let mut previous_was_colon = false;
    for ch in value.chars() {
        if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{FEFF}') {
            return false;
        }
        if matches!(
            ch,
            '#' | '['
                | ']'
                | '{'
                | '}'
                | ','
                | '\''
                | '"'
                | '*'
                | '&'
                | '!'
                | '|'
                | '>'
                | '%'
                | '@'
                | '`'
                | '?'
        ) {
            return false;
        }
        if previous_was_colon && ch.is_whitespace() {
            return false;
        }
        previous_was_colon = ch == ':';
    }

    true
}

fn is_yaml_reserved_word(value: &str) -> bool {
    value == "~"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("null")
        || value.eq_ignore_ascii_case(".nan")
        || value.eq_ignore_ascii_case(".inf")
        || value.eq_ignore_ascii_case("+.inf")
        || value.eq_ignore_ascii_case("-.inf")
}

fn write_double_quoted(output: &mut BoundedOutput, value: &str) -> fmt::Result {
    output.write_char('"')?;
    for ch in value.chars() {
        match ch {
            '\\' => output.write_str("\\\\")?,
            '"' => output.write_str("\\\"")?,
            '\0' => output.write_str("\\0")?,
            '\x07' => output.write_str("\\a")?,
            '\x08' => output.write_str("\\b")?,
            '\t' => output.write_str("\\t")?,
            '\n' => output.write_str("\\n")?,
            '\x0B' => output.write_str("\\v")?,
            '\x0C' => output.write_str("\\f")?,
            '\r' => output.write_str("\\r")?,
            '\x1B' => output.write_str("\\e")?,
            '\u{0085}' => output.write_str("\\N")?,
            '\u{2028}' => output.write_str("\\L")?,
            '\u{2029}' => output.write_str("\\P")?,
            c if (c as u32) < 0x20 => write!(output, "\\x{:02X}", c as u32)?,
            c if c.is_control() || c == '\u{FEFF}' => write!(output, "\\u{:04X}", c as u32)?,
            c => output.write_char(c)?,
        }
    }
    output.write_char('"')
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        ConfigFormat, ConfigSourceError, MAX_OUTPUT_BYTES, MAX_STRING_BYTES, decode_value,
        render_value,
    };

    #[test]
    fn renders_deterministic_sorted_minimalist_yaml() {
        let value = json!({
            "z": [true, null, 1.5, 42],
            "a": {
                "two": "forward-cert",
                "one": "1.3",
                "empty_map": {},
                "empty_list": []
            }
        });
        let rendered = render_value(ConfigFormat::Yaml, &value).unwrap();
        assert_eq!(
            rendered,
            "a:\n  empty_list: []\n  empty_map: {}\n  one: \"1.3\"\n  two: forward-cert\nz:\n  - true\n  - null\n  - 1.5\n  - 42\n"
        );
        assert_eq!(
            decode_value(ConfigFormat::Yaml, rendered.as_bytes()).unwrap(),
            value
        );
    }

    #[test]
    fn round_trips_root_scalars_and_empty_containers() {
        for value in [
            json!(null),
            json!(true),
            json!(false),
            json!(0),
            json!(-42),
            json!(3.25),
            json!(""),
            json!("plain-value"),
            json!("true"),
            json!({}),
            json!([]),
            json!([[1, 2], [{"b": 2, "a": 1}, {}]]),
        ] {
            let rendered = render_value(ConfigFormat::Yaml, &value).unwrap();
            assert!(rendered.ends_with('\n'));
            let decoded = decode_value(ConfigFormat::Yaml, rendered.as_bytes()).unwrap();
            assert_eq!(decoded, value, "rendered:\n{rendered}");
        }
    }

    #[test]
    fn enforces_the_shared_output_bound() {
        let oversized = "x".repeat(MAX_STRING_BYTES);
        let value = Value::Object(
            (0..=MAX_OUTPUT_BYTES / MAX_STRING_BYTES)
                .map(|key| (format!("key-{key}"), Value::String(oversized.clone())))
                .collect(),
        );
        assert!(matches!(
            render_value(ConfigFormat::Yaml, &value),
            Err(ConfigSourceError::OutputTooLarge)
        ));
    }
}
