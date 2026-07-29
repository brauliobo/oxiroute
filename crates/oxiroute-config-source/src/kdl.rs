use std::collections::HashSet;
use std::fmt::Write as _;

use kdl::{KdlDocument, KdlNode, KdlValue};
use serde_json::{Map, Number, Value};

use crate::ConfigSourceError;
use crate::limits::{MAX_NODES, MAX_STRUCTURAL_DEPTH, check_string, validate_value};

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let document = KdlDocument::parse(source)
        .map_err(|error| ConfigSourceError::parse("KDL 2", error.to_string()))?;
    let mut nodes = 0;
    let value = Value::Object(decode_object(document.nodes(), 0, &mut nodes)?);
    validate_value(&value)?;
    Ok(value)
}

fn decode_object(
    nodes: &[KdlNode],
    depth: usize,
    count: &mut usize,
) -> Result<Map<String, Value>, ConfigSourceError> {
    check_depth(depth)?;
    let mut names = HashSet::with_capacity(nodes.len());
    let mut object = Map::with_capacity(nodes.len());
    for node in nodes {
        let name = node.name().value();
        check_string(name)?;
        if !names.insert(name.to_owned()) {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!("duplicate object node `{name}`"),
            ));
        }
        object.insert(name.to_owned(), decode_node(node, depth + 1, count)?);
    }
    Ok(object)
}

fn decode_node(
    node: &KdlNode,
    depth: usize,
    count: &mut usize,
) -> Result<Value, ConfigSourceError> {
    check_depth(depth)?;
    *count = count.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *count > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    if node.entries().iter().any(|entry| entry.name().is_some()) {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            format!(
                "properties are not allowed on node `{}`",
                node.name().value()
            ),
        ));
    }

    let annotation = node.ty().map(kdl::KdlIdentifier::value);
    match annotation {
        Some("object") => {
            require_container_shape(node, "object")?;
            let children = node.children().expect("container shape requires children");
            Ok(Value::Object(decode_object(
                children.nodes(),
                depth,
                count,
            )?))
        }
        Some("array") => {
            require_container_shape(node, "array")?;
            let children = node.children().expect("container shape requires children");
            let mut values = Vec::with_capacity(children.nodes().len());
            for child in children.nodes() {
                if child.name().value() != "-" {
                    return Err(ConfigSourceError::parse(
                        "KDL 2",
                        format!("array child `{}` must be named `-`", child.name().value()),
                    ));
                }
                values.push(decode_node(child, depth + 1, count)?);
            }
            Ok(Value::Array(values))
        }
        Some(other) => Err(ConfigSourceError::parse(
            "KDL 2",
            format!("unsupported node type annotation `({other})`"),
        )),
        None => decode_scalar(node),
    }
}

fn require_container_shape(node: &KdlNode, kind: &str) -> Result<(), ConfigSourceError> {
    if !node.entries().is_empty() || node.children().is_none() {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            format!(
                "({kind}) node `{}` must have only a children block",
                node.name().value()
            ),
        ));
    }
    Ok(())
}

fn decode_scalar(node: &KdlNode) -> Result<Value, ConfigSourceError> {
    if node.children().is_some() || node.entries().len() != 1 {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            format!(
                "scalar node `{}` must have exactly one positional argument and no children",
                node.name().value()
            ),
        ));
    }
    if node.entries()[0].ty().is_some() {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            format!(
                "typed scalar arguments are not allowed on node `{}`",
                node.name().value()
            ),
        ));
    }
    match node.entries()[0].value() {
        KdlValue::String(value) => {
            check_string(value)?;
            Ok(Value::String(value.clone()))
        }
        KdlValue::Integer(value) => integer_value(*value),
        KdlValue::Float(value) => Number::from_f64(*value).map_or_else(
            || Err(ConfigSourceError::parse("KDL 2", "non-finite number")),
            |number| Ok(Value::Number(number)),
        ),
        KdlValue::Bool(value) => Ok(Value::Bool(*value)),
        KdlValue::Null => Ok(Value::Null),
    }
}

fn integer_value(value: i128) -> Result<Value, ConfigSourceError> {
    if let Ok(value) = i64::try_from(value) {
        return Ok(Value::Number(value.into()));
    }
    if let Ok(value) = u64::try_from(value) {
        return Ok(Value::Number(value.into()));
    }
    Err(ConfigSourceError::parse(
        "KDL 2",
        "integer is outside the JSON integer range",
    ))
}

fn check_depth(depth: usize) -> Result<(), ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    Ok(())
}

pub(crate) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    let Value::Object(object) = value else {
        return Err(ConfigSourceError::render(
            "KDL 2",
            "the documented KDL mapping requires an object root",
        ));
    };
    let mut output = String::new();
    render_object(object, 0, &mut output)?;
    Ok(output)
}

fn render_object(
    object: &Map<String, Value>,
    depth: usize,
    output: &mut String,
) -> Result<(), ConfigSourceError> {
    let mut entries = object.iter().collect::<Vec<_>>();
    entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (key, value) in entries {
        render_node(key, value, depth, output)?;
    }
    Ok(())
}

fn render_node(
    name: &str,
    value: &Value,
    depth: usize,
    output: &mut String,
) -> Result<(), ConfigSourceError> {
    let indent = "  ".repeat(depth);
    let name = kdl::KdlIdentifier::from(name).to_string();
    match value {
        Value::Object(object) => {
            writeln!(output, "{indent}(object){name} {{").expect("writing to String cannot fail");
            render_object(object, depth + 1, output)?;
            writeln!(output, "{indent}}}").expect("writing to String cannot fail");
        }
        Value::Array(values) => {
            writeln!(output, "{indent}(array){name} {{").expect("writing to String cannot fail");
            for value in values {
                render_node("-", value, depth + 1, output)?;
            }
            writeln!(output, "{indent}}}").expect("writing to String cannot fail");
        }
        value => {
            let scalar = scalar_text(value)?;
            writeln!(output, "{indent}{name} {scalar}").expect("writing to String cannot fail");
        }
    }
    Ok(())
}

fn scalar_text(value: &Value) -> Result<String, ConfigSourceError> {
    match value {
        Value::Null => Ok(KdlValue::Null.to_string()),
        Value::Bool(value) => Ok(KdlValue::Bool(*value).to_string()),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(KdlValue::Integer(i128::from(value)).to_string())
            } else if let Some(value) = value.as_u64() {
                Ok(KdlValue::Integer(i128::from(value)).to_string())
            } else {
                value.as_f64().map_or_else(
                    || Err(ConfigSourceError::render("KDL 2", "invalid JSON number")),
                    |value| Ok(KdlValue::Float(value).to_string()),
                )
            }
        }
        Value::String(value) => serde_json::to_string(value)
            .map_err(|error| ConfigSourceError::render("KDL 2", error.to_string())),
        Value::Array(_) | Value::Object(_) => unreachable!("containers are rendered separately"),
    }
}
