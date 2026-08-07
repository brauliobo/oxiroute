use std::{collections::BTreeMap, fmt::Write as _, io};

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
/// Maximum substitution references in one HOCON source document.
pub const MAX_SUBSTITUTIONS: usize = 4_096;
/// Maximum native dependency paths retained for diagnostics and watching.
pub const MAX_DEPENDENCY_PATHS: usize = 4_096;

pub(crate) struct BoundedOutput {
    output: String,
    exceeded: bool,
}

impl BoundedOutput {
    pub(crate) fn new() -> Self {
        Self {
            output: String::new(),
            exceeded: false,
        }
    }

    pub(crate) fn finish(self) -> String {
        self.output
    }

    pub(crate) fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let Some(length) = self.output.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(());
        };
        if length > MAX_OUTPUT_BYTES {
            self.exceeded = true;
            return Err(());
        }
        let value = std::str::from_utf8(bytes).map_err(|_| ())?;
        self.output.push_str(value);
        Ok(())
    }
}

impl std::fmt::Write for BoundedOutput {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.append(value.as_bytes()).map_err(|()| std::fmt::Error)
    }
}

impl io::Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.append(bytes) {
            Ok(()) => Ok(bytes.len()),
            Err(()) => Err(io::Error::other("bounded output limit exceeded")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

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
    let mut output = BoundedOutput::new();
    serde_json::to_writer_pretty(&mut output, &sorted).map_err(|error| {
        if output.exceeded() {
            ConfigSourceError::OutputTooLarge
        } else {
            ConfigSourceError::render("HOCON", error.to_string())
        }
    })?;
    writeln!(output).map_err(|_| ConfigSourceError::OutputTooLarge)?;
    Ok(output.finish())
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
