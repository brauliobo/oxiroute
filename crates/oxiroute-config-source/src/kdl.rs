use std::collections::HashSet;
use std::fmt::Write as _;

use kdl::{KdlDocument, KdlNode, KdlValue};
use serde_json::{Map, Number, Value};

use crate::ConfigSourceError;
use crate::limits::{MAX_NODES, MAX_STRUCTURAL_DEPTH, check_string, validate_value};
use crate::native::{
    NativeDirective, decode_apache, decode_haproxy, decode_nginx, decode_squid, decode_varnish,
};

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let document = KdlDocument::parse(source)
        .map_err(|error| ConfigSourceError::parse("KDL 2", error.to_string()))?;
    let mut nodes = 0;
    let value = Value::Object(decode_object(document.nodes(), 0, &mut nodes)?);
    validate_value(&value)?;
    Ok(value)
}

pub(crate) fn decode_with_directives(
    source: &str,
) -> Result<(Value, Vec<NativeDirective>), ConfigSourceError> {
    let document = KdlDocument::parse(source)
        .map_err(|error| ConfigSourceError::parse("KDL 2", error.to_string()))?;
    let mut generic_nodes = Vec::new();
    let mut directives = Vec::new();
    let mut nodes = 0;
    for node in document.nodes() {
        match node.name().value() {
            "nginx_server" => directives.push(decode_nginx_node(node, &mut nodes)?),
            "haproxy_server" => directives.push(decode_haproxy_node(node, &mut nodes)?),
            "squid_server" => directives.push(decode_squid_node(node, &mut nodes)?),
            "apache_server" => directives.push(decode_apache_node(node, &mut nodes)?),
            "varnish_server" => directives.push(decode_varnish_node(node, &mut nodes)?),
            _ => generic_nodes.push(node.clone()),
        }
    }
    let value = Value::Object(decode_object(&generic_nodes, 0, &mut nodes)?);
    validate_value(&value)?;
    Ok((value, directives))
}

fn decode_squid_node(
    node: &KdlNode,
    count: &mut usize,
) -> Result<NativeDirective, ConfigSourceError> {
    increment_node_count(count)?;
    let paths = directive_paths(node)?;
    if paths.len() != 1 {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "squid_server requires exactly one string path argument",
        ));
    }
    let mut object = decode_option_children(node, count)?;
    if object.contains_key("path") {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "squid_server path must be a positional argument",
        ));
    }
    object.insert("path".to_owned(), Value::String(paths[0].clone()));
    decode_squid(Value::Object(object), "KDL 2").map(NativeDirective::Squid)
}

fn decode_apache_node(
    node: &KdlNode,
    count: &mut usize,
) -> Result<NativeDirective, ConfigSourceError> {
    increment_node_count(count)?;
    let paths = directive_paths(node)?;
    if paths.len() != 1 {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "apache_server requires exactly one string path argument",
        ));
    }
    let mut object = decode_option_children(node, count)?;
    if object.contains_key("path") {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "apache_server path must be a positional argument",
        ));
    }
    object.insert("path".to_owned(), Value::String(paths[0].clone()));
    decode_apache(Value::Object(object), "KDL 2").map(NativeDirective::Apache)
}

fn decode_varnish_node(
    node: &KdlNode,
    count: &mut usize,
) -> Result<NativeDirective, ConfigSourceError> {
    increment_node_count(count)?;
    let arguments = directive_paths(node)?;
    let Some((path, invocation)) = arguments.split_first() else {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "varnish_server requires a VCL path argument",
        ));
    };
    if node.children().is_some() {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "varnish_server invocation arguments must be positional strings",
        ));
    }
    let object = serde_json::json!({
        "path": path,
        "arguments": invocation,
    });
    decode_varnish(object, "KDL 2").map(NativeDirective::Varnish)
}

fn decode_nginx_node(
    node: &KdlNode,
    count: &mut usize,
) -> Result<NativeDirective, ConfigSourceError> {
    increment_node_count(count)?;
    let paths = directive_paths(node)?;
    if paths.len() != 1 {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "nginx_server requires exactly one string path argument",
        ));
    }
    let mut object = decode_option_children(node, count)?;
    if object.contains_key("path") {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "nginx_server path must be a positional argument",
        ));
    }
    object.insert("path".to_owned(), Value::String(paths[0].clone()));
    decode_nginx(Value::Object(object), "KDL 2").map(NativeDirective::Nginx)
}

fn decode_haproxy_node(
    node: &KdlNode,
    count: &mut usize,
) -> Result<NativeDirective, ConfigSourceError> {
    increment_node_count(count)?;
    let paths = directive_paths(node)?;
    if paths.is_empty() {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "haproxy_server requires at least one string path argument",
        ));
    }
    let mut object = decode_option_children(node, count)?;
    if object.contains_key("paths") {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            "haproxy_server paths must be positional arguments",
        ));
    }
    object.insert(
        "paths".to_owned(),
        Value::Array(paths.into_iter().map(Value::String).collect()),
    );
    decode_haproxy(Value::Object(object), "KDL 2").map(NativeDirective::Haproxy)
}

fn directive_paths(node: &KdlNode) -> Result<Vec<String>, ConfigSourceError> {
    if node.ty().is_some() {
        return Err(ConfigSourceError::parse(
            "KDL 2",
            format!(
                "type annotations are not allowed on native directive `{}`",
                node.name().value()
            ),
        ));
    }
    let mut paths = Vec::with_capacity(node.entries().len());
    for entry in node.entries() {
        if entry.name().is_some() {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!(
                    "properties are not allowed on native directive `{}`",
                    node.name().value()
                ),
            ));
        }
        if entry.ty().is_some() {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!(
                    "typed path arguments are not allowed on native directive `{}`",
                    node.name().value()
                ),
            ));
        }
        let KdlValue::String(path) = entry.value() else {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!(
                    "native directive `{}` path arguments must be strings",
                    node.name().value()
                ),
            ));
        };
        check_string(path)?;
        paths.push(path.clone());
    }
    Ok(paths)
}

fn decode_option_children(
    node: &KdlNode,
    count: &mut usize,
) -> Result<Map<String, Value>, ConfigSourceError> {
    let mut options = Map::new();
    for child in node.children().map_or(&[][..], |children| children.nodes()) {
        increment_node_count(count)?;
        let name = child.name().value();
        check_string(name)?;
        if child.ty().is_some() {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!("type annotations are not allowed on native option `{name}`"),
            ));
        }
        if child.entries().iter().any(|entry| entry.name().is_some()) {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!("properties are not allowed on native option `{name}`"),
            ));
        }
        if options.contains_key(name) {
            return Err(ConfigSourceError::parse(
                "KDL 2",
                format!("duplicate native option `{name}`"),
            ));
        }
        options.insert(name.to_owned(), decode_scalar(child)?);
    }
    Ok(options)
}

fn increment_node_count(count: &mut usize) -> Result<(), ConfigSourceError> {
    *count = count.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *count > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    Ok(())
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
    increment_node_count(count)?;
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
