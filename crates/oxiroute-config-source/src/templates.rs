use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::ConfigSourceError;
use crate::limits::{MAX_EXPANSION_DEPTH, MAX_OUTPUT_BYTES, validate_value};

/// Expands declarative `templates` and `use` markers in a value tree.
///
/// Template names in a `use` array are applied from left to right, so later templates override
/// earlier templates. Local fields override all templates. Object values merge recursively;
/// arrays, scalars, and null replace inherited values.
///
/// # Errors
///
/// Returns an error for malformed markers, unknown or cyclic templates, non-object templates, or
/// values and expanded output that exceed the fixed safety bounds.
pub fn expand_templates(value: &Value) -> Result<Value, ConfigSourceError> {
    validate_value(value)?;
    let mut root = value.clone();
    let templates = match &mut root {
        Value::Object(object) => match object.remove("templates") {
            None => Map::new(),
            Some(Value::Object(templates)) => templates,
            Some(_) => {
                return Err(ConfigSourceError::Template(
                    "root `templates` must be an object".to_owned(),
                ));
            }
        },
        _ => Map::new(),
    };
    for (name, template) in &templates {
        if !template.is_object() {
            return Err(ConfigSourceError::Template(format!(
                "template `{name}` must be an object"
            )));
        }
    }

    let mut resolver = Expander {
        templates,
        resolved: HashMap::new(),
        stack: Vec::new(),
    };
    let expanded_value = resolver.expand_value(&root, 0)?;
    validate_value(&expanded_value)?;
    let output_size = serde_json::to_vec(&expanded_value)
        .map_err(|error| ConfigSourceError::Template(error.to_string()))?
        .len();
    if output_size > MAX_OUTPUT_BYTES {
        return Err(ConfigSourceError::OutputTooLarge);
    }
    Ok(expanded_value)
}

struct Expander {
    templates: Map<String, Value>,
    resolved: HashMap<String, Value>,
    stack: Vec<String>,
}

impl Expander {
    fn expand_value(
        &mut self,
        value: &Value,
        inheritance_depth: usize,
    ) -> Result<Value, ConfigSourceError> {
        match value {
            Value::Object(object) => self.expand_object(object, inheritance_depth),
            Value::Array(values) => values
                .iter()
                .map(|value| self.expand_value(value, inheritance_depth))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            value => Ok(value.clone()),
        }
    }

    fn expand_object(
        &mut self,
        object: &Map<String, Value>,
        inheritance_depth: usize,
    ) -> Result<Value, ConfigSourceError> {
        let uses = parse_uses(object.get("use"))?;
        let mut merged = Map::new();
        for name in uses {
            let template = self.resolve_template(&name, inheritance_depth + 1)?;
            let Value::Object(template) = template else {
                return Err(ConfigSourceError::Template(format!(
                    "template `{name}` did not expand to an object"
                )));
            };
            merge_objects(&mut merged, template);
        }

        let mut local = Map::new();
        for (key, value) in object {
            if key != "use" {
                local.insert(key.clone(), self.expand_value(value, inheritance_depth)?);
            }
        }
        merge_objects(&mut merged, local);
        Ok(Value::Object(merged))
    }

    fn resolve_template(
        &mut self,
        name: &str,
        inheritance_depth: usize,
    ) -> Result<Value, ConfigSourceError> {
        if inheritance_depth > MAX_EXPANSION_DEPTH {
            return Err(ConfigSourceError::ExpansionDepth);
        }
        if let Some(value) = self.resolved.get(name) {
            return Ok(value.clone());
        }
        if let Some(position) = self.stack.iter().position(|entry| entry == name) {
            let mut cycle = self.stack[position..].to_vec();
            cycle.push(name.to_owned());
            return Err(ConfigSourceError::Template(format!(
                "template cycle: {}",
                cycle.join(" -> ")
            )));
        }
        let template = self
            .templates
            .get(name)
            .cloned()
            .ok_or_else(|| ConfigSourceError::Template(format!("unknown template `{name}`")))?;
        let Value::Object(template) = template else {
            return Err(ConfigSourceError::Template(format!(
                "template `{name}` must be an object"
            )));
        };

        self.stack.push(name.to_owned());
        let expanded = self.expand_object(&template, inheritance_depth);
        self.stack.pop();
        let expanded = expanded?;
        self.resolved.insert(name.to_owned(), expanded.clone());
        Ok(expanded)
    }
}

fn parse_uses(value: Option<&Value>) -> Result<Vec<String>, ConfigSourceError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(name)) => Ok(vec![name.clone()]),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(name) => Ok(name.clone()),
                _ => Err(ConfigSourceError::Template(
                    "`use` arrays may contain only template names".to_owned(),
                )),
            })
            .collect(),
        Some(_) => Err(ConfigSourceError::Template(
            "`use` must be a template name or an array of template names".to_owned(),
        )),
    }
}

fn merge_objects(target: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        if let (Some(Value::Object(target)), Value::Object(overlay)) =
            (target.get_mut(&key), &value)
        {
            merge_objects(target, overlay.clone());
        } else {
            target.insert(key, value);
        }
    }
}
