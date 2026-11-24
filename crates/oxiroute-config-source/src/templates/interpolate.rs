use serde_json::{Map, Value};

use crate::ConfigSourceError;
use crate::limits::{
    MAX_EXPANSION_DEPTH, MAX_NODES, MAX_OUTPUT_BYTES, MAX_STRING_BYTES, MAX_STRUCTURAL_DEPTH,
    MAX_SUBSTITUTIONS, check_string,
};

const MAX_WORK_NODES: usize = MAX_NODES * 4;
const MAX_WORK_STRING_BYTES: usize = MAX_OUTPUT_BYTES * 2;

pub(super) struct ExpansionBudget {
    substitutions: usize,
    work_nodes: usize,
    work_string_bytes: usize,
}

impl ExpansionBudget {
    pub(super) const fn new() -> Self {
        Self {
            substitutions: 0,
            work_nodes: 0,
            work_string_bytes: 0,
        }
    }

    pub(super) const fn substitutions(&self) -> usize {
        self.substitutions
    }

    pub(super) fn record_substitutions(&mut self, count: usize) -> Result<(), ConfigSourceError> {
        self.substitutions = self
            .substitutions
            .checked_add(count)
            .ok_or(ConfigSourceError::SubstitutionLimit)?;
        if self.substitutions > MAX_SUBSTITUTIONS {
            return Err(ConfigSourceError::SubstitutionLimit);
        }
        Ok(())
    }

    pub(super) fn record_node(&mut self) -> Result<(), ConfigSourceError> {
        self.work_nodes = self
            .work_nodes
            .checked_add(1)
            .ok_or(ConfigSourceError::NodeLimit)?;
        if self.work_nodes > MAX_WORK_NODES {
            return Err(ConfigSourceError::NodeLimit);
        }
        Ok(())
    }

    pub(super) fn record_string_bytes(&mut self, len: usize) -> Result<(), ConfigSourceError> {
        self.work_string_bytes = self
            .work_string_bytes
            .checked_add(len)
            .ok_or(ConfigSourceError::OutputTooLarge)?;
        if self.work_string_bytes > MAX_WORK_STRING_BYTES {
            return Err(ConfigSourceError::OutputTooLarge);
        }
        Ok(())
    }

    pub(super) fn record_value(&mut self, value: &Value) -> Result<(), ConfigSourceError> {
        self.record_node()?;
        match value {
            Value::String(text) => self.record_string_bytes(text.len()),
            Value::Array(items) => {
                for item in items {
                    self.record_value(item)?;
                }
                Ok(())
            }
            Value::Object(object) => self.record_object(object),
            Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        }
    }

    pub(super) fn record_object(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<(), ConfigSourceError> {
        for (key, value) in object {
            self.record_node()?;
            self.record_string_bytes(key.len())?;
            self.record_value(value)?;
        }
        Ok(())
    }
}

pub(super) trait ScopeResolver {
    fn resolve_path(
        &mut self,
        expr: &str,
        root_key: &str,
        rest: &[&str],
        depth: usize,
        budget: &mut ExpansionBudget,
    ) -> Result<Value, ConfigSourceError>;
}

pub(super) struct StaticScope<'a> {
    pub(super) vars: &'a Map<String, Value>,
    pub(super) params: Option<&'a Map<String, Value>>,
}

impl ScopeResolver for StaticScope<'_> {
    fn resolve_path(
        &mut self,
        expr: &str,
        root_key: &str,
        rest: &[&str],
        _depth: usize,
        budget: &mut ExpansionBudget,
    ) -> Result<Value, ConfigSourceError> {
        let root_value = self
            .params
            .and_then(|params| params.get(root_key))
            .or_else(|| self.vars.get(root_key))
            .ok_or_else(|| unknown_variable_error(expr))?;
        let leaf = traverse_path(root_value, expr, rest)?;
        budget.record_value(leaf)?;
        Ok(leaf.clone())
    }
}

enum BindingLocation {
    Resolved,
    Fallback,
    Missing,
}

struct BindingResolver<'a> {
    kind: &'static str,
    raw: &'a Map<String, Value>,
    resolved: Map<String, Value>,
    fallback: Option<&'a Map<String, Value>>,
    stack: Vec<String>,
}

impl BindingResolver<'_> {
    fn ensure_resolved(
        &mut self,
        name: &str,
        depth: usize,
        budget: &mut ExpansionBudget,
    ) -> Result<BindingLocation, ConfigSourceError> {
        if self.resolved.contains_key(name) {
            return Ok(BindingLocation::Resolved);
        }
        if let Some(raw_value) = self.raw.get(name) {
            if let Some(position) = self.stack.iter().position(|entry| entry == name) {
                let mut cycle = self.stack[position..].to_vec();
                cycle.push(name.to_owned());
                return Err(ConfigSourceError::Template(format!(
                    "{} cycle: {}",
                    self.kind,
                    cycle.join(" -> ")
                )));
            }
            if depth > MAX_EXPANSION_DEPTH {
                return Err(ConfigSourceError::ExpansionDepth);
            }
            self.stack.push(name.to_owned());
            let result = interpolate_value_tree(raw_value, self, depth + 1, 0, budget);
            self.stack.pop();
            let value = result?;
            self.resolved.insert(name.to_owned(), value);
            return Ok(BindingLocation::Resolved);
        }
        if self
            .fallback
            .is_some_and(|fallback| fallback.contains_key(name))
        {
            return Ok(BindingLocation::Fallback);
        }
        Ok(BindingLocation::Missing)
    }
}

impl ScopeResolver for BindingResolver<'_> {
    fn resolve_path(
        &mut self,
        expr: &str,
        root_key: &str,
        rest: &[&str],
        depth: usize,
        budget: &mut ExpansionBudget,
    ) -> Result<Value, ConfigSourceError> {
        let root_value = match self.ensure_resolved(root_key, depth, budget)? {
            BindingLocation::Resolved => self
                .resolved
                .get(root_key)
                .ok_or_else(|| unknown_variable_error(expr))?,
            BindingLocation::Fallback => self
                .fallback
                .and_then(|fallback| fallback.get(root_key))
                .ok_or_else(|| unknown_variable_error(expr))?,
            BindingLocation::Missing => return Err(unknown_variable_error(expr)),
        };
        let leaf = traverse_path(root_value, expr, rest)?;
        budget.record_value(leaf)?;
        Ok(leaf.clone())
    }
}

pub(super) fn resolve_bindings(
    kind: &'static str,
    raw: &Map<String, Value>,
    prebound: Map<String, Value>,
    fallback: Option<&Map<String, Value>>,
    budget: &mut ExpansionBudget,
) -> Result<Map<String, Value>, ConfigSourceError> {
    let mut resolver = BindingResolver {
        kind,
        raw,
        resolved: prebound,
        fallback,
        stack: Vec::new(),
    };
    for key in raw.keys() {
        if !resolver.resolved.contains_key(key) {
            let _ = resolver.ensure_resolved(key, 1, budget)?;
        }
    }
    Ok(resolver.resolved)
}

fn interpolate_value_tree(
    value: &Value,
    scope: &mut impl ScopeResolver,
    depth: usize,
    structural_depth: usize,
    budget: &mut ExpansionBudget,
) -> Result<Value, ConfigSourceError> {
    if structural_depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    budget.record_node()?;
    match value {
        Value::String(text) => interpolate_string(text, scope, depth, budget),
        Value::Array(items) => items
            .iter()
            .map(|item| interpolate_value_tree(item, scope, depth, structural_depth + 1, budget))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object) => {
            let mut result = Map::with_capacity(object.len());
            for (key, entry) in object {
                let expanded_key = interpolate_string_only(key, scope, depth, budget)?;
                let expanded_value =
                    interpolate_value_tree(entry, scope, depth, structural_depth + 1, budget)?;
                result.insert(expanded_key, expanded_value);
            }
            Ok(Value::Object(result))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.clone()),
    }
}

pub(super) fn interpolate_string(
    input: &str,
    scope: &mut impl ScopeResolver,
    depth: usize,
    budget: &mut ExpansionBudget,
) -> Result<Value, ConfigSourceError> {
    if !input.contains("{{") {
        budget.record_string_bytes(input.len())?;
        return Ok(Value::String(input.to_owned()));
    }
    if let Some(expr) = whole_value_expr(input)? {
        budget.record_substitutions(1)?;
        let value = lookup_path(expr, scope, depth, budget)?;
        if let Value::String(text) = &value {
            check_string(text)?;
        }
        return Ok(value);
    }
    interpolate_embedded(input, scope, depth, budget).map(Value::String)
}

pub(super) fn interpolate_string_only(
    input: &str,
    scope: &mut impl ScopeResolver,
    depth: usize,
    budget: &mut ExpansionBudget,
) -> Result<String, ConfigSourceError> {
    if !input.contains("{{") {
        budget.record_string_bytes(input.len())?;
        return Ok(input.to_owned());
    }
    interpolate_embedded(input, scope, depth, budget)
}

fn whole_value_expr(input: &str) -> Result<Option<&str>, ConfigSourceError> {
    if !input.starts_with("{{") {
        return Ok(None);
    }
    let Some(close_rel) = input[2..].find("}}") else {
        return Err(unclosed_expression_error());
    };
    let close_end = 2 + close_rel + 2;
    if close_end != input.len() {
        return Ok(None);
    }
    let inner = &input[2..2 + close_rel];
    if inner.contains("{{") {
        return Err(unclosed_expression_error());
    }
    let expr = inner.trim();
    if expr.is_empty() {
        return Err(ConfigSourceError::Template(
            "empty template expression `{{}}`".to_owned(),
        ));
    }
    Ok(Some(expr))
}

fn interpolate_embedded(
    input: &str,
    scope: &mut impl ScopeResolver,
    depth: usize,
    budget: &mut ExpansionBudget,
) -> Result<String, ConfigSourceError> {
    let mut output = String::with_capacity(input.len());
    let mut pos = 0;
    let bytes = input.as_bytes();

    while pos < input.len() {
        let Some(rel) = input[pos..].find("{{") else {
            append_checked(&mut output, &input[pos..])?;
            break;
        };
        let open = pos + rel;
        if open > pos && bytes[open - 1] == b'\\' {
            append_checked(&mut output, &input[pos..open - 1])?;
            append_checked(&mut output, "{{")?;
            pos = open + 2;
            continue;
        }
        append_checked(&mut output, &input[pos..open])?;
        let after_open = open + 2;
        let Some(close_rel) = input[after_open..].find("}}") else {
            return Err(unclosed_expression_error());
        };
        let raw_expr = &input[after_open..after_open + close_rel];
        if raw_expr.contains("{{") {
            return Err(unclosed_expression_error());
        }
        let expr = raw_expr.trim();
        if expr.is_empty() {
            return Err(ConfigSourceError::Template(
                "empty template expression `{{}}`".to_owned(),
            ));
        }
        budget.record_substitutions(1)?;
        let value = lookup_path(expr, scope, depth, budget)?;
        append_scalar(&mut output, expr, &value)?;
        pos = after_open + close_rel + 2;
    }

    budget.record_string_bytes(output.len())?;
    Ok(output)
}

fn lookup_path(
    expr: &str,
    scope: &mut impl ScopeResolver,
    depth: usize,
    budget: &mut ExpansionBudget,
) -> Result<Value, ConfigSourceError> {
    let mut segments = expr.split('.');
    let root_key = segments.next().unwrap_or("");
    if !is_valid_segment(root_key) {
        return Err(invalid_variable_error(expr));
    }
    let rest: Vec<&str> = segments.collect();
    for segment in &rest {
        if !is_valid_segment(segment) {
            return Err(invalid_variable_error(expr));
        }
    }
    scope.resolve_path(expr, root_key, &rest, depth, budget)
}

fn traverse_path<'a>(
    mut current: &'a Value,
    expr: &str,
    rest: &[&str],
) -> Result<&'a Value, ConfigSourceError> {
    for segment in rest {
        let Value::Object(object) = current else {
            return Err(ConfigSourceError::Template(format!(
                "cannot resolve `{expr}`: path segment `{segment}` requires an object"
            )));
        };
        current = object
            .get(*segment)
            .ok_or_else(|| unknown_variable_error(expr))?;
    }
    Ok(current)
}

fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|ch| !ch.is_whitespace() && !matches!(ch, '{' | '}' | '\\'))
}

fn append_checked(output: &mut String, text: &str) -> Result<(), ConfigSourceError> {
    let new_len = output
        .len()
        .checked_add(text.len())
        .ok_or(ConfigSourceError::StringLimit)?;
    if new_len > MAX_STRING_BYTES {
        return Err(ConfigSourceError::StringLimit);
    }
    output.push_str(text);
    Ok(())
}

fn append_scalar(output: &mut String, expr: &str, value: &Value) -> Result<(), ConfigSourceError> {
    match value {
        Value::String(text) => append_checked(output, text),
        Value::Number(number) => append_checked(output, &number.to_string()),
        Value::Bool(boolean) => append_checked(output, if *boolean { "true" } else { "false" }),
        Value::Null | Value::Array(_) | Value::Object(_) => Err(ConfigSourceError::Template(
            format!("cannot interpolate non-scalar variable `{expr}` into string"),
        )),
    }
}

fn unknown_variable_error(expr: &str) -> ConfigSourceError {
    ConfigSourceError::Template(format!("unknown template variable `{expr}`"))
}

fn invalid_variable_error(expr: &str) -> ConfigSourceError {
    ConfigSourceError::Template(format!("invalid template variable `{expr}`"))
}

fn unclosed_expression_error() -> ConfigSourceError {
    ConfigSourceError::Template("unclosed template expression `{{`".to_owned())
}
