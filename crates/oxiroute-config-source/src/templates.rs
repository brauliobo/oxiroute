mod interpolate;

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::ConfigSourceError;
use crate::limits::{BoundedOutput, MAX_EXPANSION_DEPTH, MAX_STRUCTURAL_DEPTH, validate_value};
use interpolate::{
    ExpansionBudget, StaticScope, interpolate_string, interpolate_string_only, resolve_bindings,
};

/// Expands declarative `vars`, `templates`, parameterized `use` markers, and `{{var}}`
/// interpolation in a value tree.
///
/// Optional root `vars` defines reusable values (which may reference other `vars` via `{{name}}`).
/// Optional root `templates` defines reusable objects that may declare default `params`. Both
/// `vars` and `templates` are removed from the expanded root, and `params` is removed from
/// expanded templates.
///
/// `use` accepts a template name string, a parameterized object (`{ "name": "...", ... }` or
/// `{ "template": "...", "with": { ... } }`), or an array mixing both forms. Templates in a `use`
/// array are applied from left to right, so later templates override earlier templates. Local
/// fields override all templates. Object values merge recursively; arrays, scalars, and null
/// replace inherited values.
///
/// String interpolation uses `{{ident}}` (including dotted lookups like `{{edge.port}}`), where
/// template parameters shadow root `vars`. A string that is solely `"{{ident}}"` evaluates to the
/// typed JSON value; embedded `{{ident}}` formats scalars into the surrounding string. Use `\{{`
/// to emit a literal `{{`.
///
/// # Errors
///
/// Returns an error for malformed markers, unknown or cyclic templates or variables, unclosed or
/// invalid `{{...}}` expressions, non-object `vars`/`templates`/`params`, or values and expanded
/// output that exceed the fixed safety bounds.
pub fn expand_templates(value: &Value) -> Result<Value, ConfigSourceError> {
    validate_value(value)?;
    let mut root = value.clone();
    let RootSections {
        templates,
        vars: raw_vars,
    } = extract_root_sections(&mut root)?;

    let mut budget = ExpansionBudget::new();
    let vars = resolve_bindings("variable", &raw_vars, Map::new(), None, &mut budget)?;

    let mut resolver = Expander {
        templates,
        vars,
        resolved_unparameterized: HashMap::new(),
        stack: Vec::new(),
        budget,
        current_max_depth: 0,
    };
    let expanded_value = resolver.expand_value(&root, 0, 0, None)?;
    validate_value(&expanded_value)?;
    let mut output = BoundedOutput::new();
    serde_json::to_writer(&mut output, &expanded_value).map_err(|error| {
        if output.exceeded() {
            ConfigSourceError::OutputTooLarge
        } else {
            ConfigSourceError::Template(error.to_string())
        }
    })?;
    Ok(expanded_value)
}

struct RootSections {
    templates: Map<String, Value>,
    vars: Map<String, Value>,
}

fn extract_root_sections(root: &mut Value) -> Result<RootSections, ConfigSourceError> {
    let Value::Object(object) = root else {
        return Ok(RootSections {
            templates: Map::new(),
            vars: Map::new(),
        });
    };
    let templates = match object.remove("templates") {
        None => Map::new(),
        Some(Value::Object(templates)) => templates,
        Some(_) => {
            return Err(ConfigSourceError::Template(
                "root `templates` must be an object".to_owned(),
            ));
        }
    };
    let vars = match object.remove("vars") {
        None => Map::new(),
        Some(Value::Object(vars)) => vars,
        Some(_) => {
            return Err(ConfigSourceError::Template(
                "root `vars` must be an object".to_owned(),
            ));
        }
    };
    for (name, template) in &templates {
        let Value::Object(template_object) = template else {
            return Err(ConfigSourceError::Template(format!(
                "template `{name}` must be an object"
            )));
        };
        if let Some(params) = template_object.get("params")
            && !params.is_object()
        {
            return Err(ConfigSourceError::Template(format!(
                "template `{name}` `params` must be an object"
            )));
        }
    }
    Ok(RootSections { templates, vars })
}

#[derive(Clone)]
struct CachedTemplate {
    object: Map<String, Value>,
    substitutions: usize,
    max_relative_depth: usize,
}

struct UseSpec {
    name: String,
    args: Map<String, Value>,
}

struct Expander {
    templates: Map<String, Value>,
    vars: Map<String, Value>,
    resolved_unparameterized: HashMap<String, CachedTemplate>,
    stack: Vec<String>,
    budget: ExpansionBudget,
    current_max_depth: usize,
}

impl Expander {
    fn expand_value(
        &mut self,
        value: &Value,
        inheritance_depth: usize,
        structural_depth: usize,
        params: Option<&Map<String, Value>>,
    ) -> Result<Value, ConfigSourceError> {
        if structural_depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        self.budget.record_node()?;
        match value {
            Value::Object(object) => {
                self.expand_object(object, inheritance_depth, structural_depth, params, false)
            }
            Value::Array(values) => values
                .iter()
                .map(|entry| {
                    self.expand_value(entry, inheritance_depth, structural_depth + 1, params)
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::String(text) => {
                let mut scope = StaticScope {
                    vars: &self.vars,
                    params,
                };
                interpolate_string(text, &mut scope, inheritance_depth, &mut self.budget)
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.clone()),
        }
    }

    fn expand_object(
        &mut self,
        object: &Map<String, Value>,
        inheritance_depth: usize,
        structural_depth: usize,
        params: Option<&Map<String, Value>>,
        strip_params: bool,
    ) -> Result<Value, ConfigSourceError> {
        let uses = self.parse_uses(
            object.get("use"),
            inheritance_depth,
            structural_depth,
            params,
        )?;
        let mut merged = Map::new();
        for spec in uses {
            let template = self.resolve_template(spec, inheritance_depth + 1, structural_depth)?;
            merge_objects(&mut merged, template);
        }

        let mut local = Map::new();
        for (key, value) in object {
            if key == "use" || (strip_params && key == "params") {
                continue;
            }
            let expanded_key = {
                let mut scope = StaticScope {
                    vars: &self.vars,
                    params,
                };
                interpolate_string_only(key, &mut scope, inheritance_depth, &mut self.budget)?
            };
            let expanded_value =
                self.expand_value(value, inheritance_depth, structural_depth + 1, params)?;
            local.insert(expanded_key, expanded_value);
        }
        merge_objects(&mut merged, local);
        Ok(Value::Object(merged))
    }

    fn resolve_template(
        &mut self,
        spec: UseSpec,
        inheritance_depth: usize,
        structural_depth: usize,
    ) -> Result<Map<String, Value>, ConfigSourceError> {
        if inheritance_depth > MAX_EXPANSION_DEPTH {
            return Err(ConfigSourceError::ExpansionDepth);
        }
        if let Some(position) = self.stack.iter().position(|entry| entry == &spec.name) {
            let mut cycle = self.stack[position..].to_vec();
            cycle.push(spec.name);
            return Err(ConfigSourceError::Template(format!(
                "template cycle: {}",
                cycle.join(" -> ")
            )));
        }

        let is_unparameterized = spec.args.is_empty();
        if is_unparameterized && let Some(cached) = self.resolved_unparameterized.get(&spec.name) {
            let max_depth = inheritance_depth.saturating_add(cached.max_relative_depth);
            if max_depth > MAX_EXPANSION_DEPTH {
                return Err(ConfigSourceError::ExpansionDepth);
            }
            self.current_max_depth = self.current_max_depth.max(max_depth);
            self.budget.record_substitutions(cached.substitutions)?;
            self.budget.record_object(&cached.object)?;
            return Ok(cached.object.clone());
        }

        let template = self.templates.get(&spec.name).cloned().ok_or_else(|| {
            ConfigSourceError::Template(format!("unknown template `{}`", spec.name))
        })?;
        let Value::Object(template) = template else {
            return Err(ConfigSourceError::Template(format!(
                "template `{}` must be an object",
                spec.name
            )));
        };

        let raw_params = match template.get("params") {
            None => None,
            Some(Value::Object(params)) => Some(params),
            Some(_) => {
                return Err(ConfigSourceError::Template(format!(
                    "template `{}` `params` must be an object",
                    spec.name
                )));
            }
        };

        let subst_before = self.budget.substitutions();
        let prev_max_depth = self.current_max_depth;
        self.current_max_depth = inheritance_depth;

        let effective_params = match (raw_params, spec.args.is_empty()) {
            (None, true) => None,
            (None, false) => Some(spec.args),
            (Some(raw_params), _) => Some(resolve_bindings(
                "parameter",
                raw_params,
                spec.args,
                Some(&self.vars),
                &mut self.budget,
            )?),
        };

        self.stack.push(spec.name.clone());
        let expanded = self.expand_object(
            &template,
            inheritance_depth,
            structural_depth,
            effective_params.as_ref(),
            true,
        );
        self.stack.pop();

        let child_max_depth = self.current_max_depth;
        self.current_max_depth = prev_max_depth.max(child_max_depth);

        let Value::Object(expanded_map) = expanded? else {
            return Err(ConfigSourceError::Template(format!(
                "template `{}` did not expand to an object",
                spec.name
            )));
        };

        if is_unparameterized {
            let substitutions = self.budget.substitutions().saturating_sub(subst_before);
            let max_relative_depth = child_max_depth.saturating_sub(inheritance_depth);
            self.resolved_unparameterized.insert(
                spec.name,
                CachedTemplate {
                    object: expanded_map.clone(),
                    substitutions,
                    max_relative_depth,
                },
            );
        }
        Ok(expanded_map)
    }

    fn parse_uses(
        &mut self,
        value: Option<&Value>,
        inheritance_depth: usize,
        structural_depth: usize,
        params: Option<&Map<String, Value>>,
    ) -> Result<Vec<UseSpec>, ConfigSourceError> {
        match value {
            None => Ok(Vec::new()),
            Some(Value::String(raw)) => {
                if !raw.contains("{{") {
                    return Ok(vec![UseSpec {
                        name: raw.clone(),
                        args: Map::new(),
                    }]);
                }
                let resolved = {
                    let mut scope = StaticScope {
                        vars: &self.vars,
                        params,
                    };
                    interpolate_string(raw, &mut scope, inheritance_depth, &mut self.budget)?
                };
                self.parse_resolved_uses(&resolved, inheritance_depth, structural_depth, params)
            }
            Some(Value::Object(object)) => Ok(vec![self.parse_use_object(
                object,
                inheritance_depth,
                structural_depth,
                params,
                true,
            )?]),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    self.parse_use_array_item(item, inheritance_depth, structural_depth, params)
                })
                .collect(),
            Some(_) => Err(ConfigSourceError::Template(
                "`use` must be a template name, a template call object, or an array of them"
                    .to_owned(),
            )),
        }
    }

    fn parse_resolved_uses(
        &mut self,
        resolved: &Value,
        inheritance_depth: usize,
        structural_depth: usize,
        params: Option<&Map<String, Value>>,
    ) -> Result<Vec<UseSpec>, ConfigSourceError> {
        match resolved {
            Value::String(name) => Ok(vec![UseSpec {
                name: name.clone(),
                args: Map::new(),
            }]),
            Value::Object(object) => Ok(vec![self.parse_use_object(
                object,
                inheritance_depth,
                structural_depth,
                params,
                false,
            )?]),
            Value::Array(items) => items
                .iter()
                .map(|item| match item {
                    Value::String(name) => Ok(UseSpec {
                        name: name.clone(),
                        args: Map::new(),
                    }),
                    Value::Object(object) => self.parse_use_object(
                        object,
                        inheritance_depth,
                        structural_depth,
                        params,
                        false,
                    ),
                    _ => Err(ConfigSourceError::Template(
                        "`use` arrays may contain only template names or template call objects"
                            .to_owned(),
                    )),
                })
                .collect(),
            _ => Err(ConfigSourceError::Template(
                "`use` must be a template name, a template call object, or an array of them"
                    .to_owned(),
            )),
        }
    }

    fn parse_use_array_item(
        &mut self,
        item: &Value,
        inheritance_depth: usize,
        structural_depth: usize,
        params: Option<&Map<String, Value>>,
    ) -> Result<UseSpec, ConfigSourceError> {
        match item {
            Value::String(raw) => {
                if !raw.contains("{{") {
                    return Ok(UseSpec {
                        name: raw.clone(),
                        args: Map::new(),
                    });
                }
                let resolved = {
                    let mut scope = StaticScope {
                        vars: &self.vars,
                        params,
                    };
                    interpolate_string(raw, &mut scope, inheritance_depth, &mut self.budget)?
                };
                match resolved {
                    Value::String(name) => Ok(UseSpec {
                        name,
                        args: Map::new(),
                    }),
                    Value::Object(object) => self.parse_use_object(
                        &object,
                        inheritance_depth,
                        structural_depth,
                        params,
                        false,
                    ),
                    _ => Err(ConfigSourceError::Template(
                        "`use` arrays may contain only template names or template call objects"
                            .to_owned(),
                    )),
                }
            }
            Value::Object(object) => {
                self.parse_use_object(object, inheritance_depth, structural_depth, params, true)
            }
            _ => Err(ConfigSourceError::Template(
                "`use` arrays may contain only template names or template call objects".to_owned(),
            )),
        }
    }

    fn parse_use_object(
        &mut self,
        object: &Map<String, Value>,
        inheritance_depth: usize,
        structural_depth: usize,
        params: Option<&Map<String, Value>>,
        expand_values: bool,
    ) -> Result<UseSpec, ConfigSourceError> {
        let (selector_key, raw_name) = match (object.get("name"), object.get("template")) {
            (Some(_), Some(_)) => {
                return Err(ConfigSourceError::Template(
                    "`use` object cannot specify both `name` and `template`".to_owned(),
                ));
            }
            (Some(value), None) => ("name", value),
            (None, Some(value)) => ("template", value),
            (None, None) => {
                return Err(ConfigSourceError::Template(
                    "`use` object must contain a string `name` or `template`".to_owned(),
                ));
            }
        };

        let resolved_name = if expand_values {
            self.expand_value(raw_name, inheritance_depth, structural_depth + 1, params)?
        } else {
            raw_name.clone()
        };
        let Value::String(name) = resolved_name else {
            return Err(ConfigSourceError::Template(format!(
                "`use.{selector_key}` must be a string"
            )));
        };

        let mut args = Map::new();
        let mut with_unpacked = false;
        if let Some(raw_with) = object.get("with") {
            let expanded_with = if expand_values {
                self.expand_value(raw_with, inheritance_depth, structural_depth + 1, params)?
            } else {
                raw_with.clone()
            };
            match expanded_with {
                Value::Object(with_map) => {
                    args.extend(with_map);
                    with_unpacked = true;
                }
                _ if selector_key == "template" => {
                    return Err(ConfigSourceError::Template(
                        "`use.with` must be an object".to_owned(),
                    ));
                }
                other => {
                    args.insert("with".to_owned(), other);
                    with_unpacked = true;
                }
            }
        }

        for (key, value) in object {
            if key == selector_key || (with_unpacked && key == "with") {
                continue;
            }
            let expanded_key = if expand_values {
                let mut scope = StaticScope {
                    vars: &self.vars,
                    params,
                };
                interpolate_string_only(key, &mut scope, inheritance_depth, &mut self.budget)?
            } else {
                key.clone()
            };
            let expanded_value = if expand_values {
                self.expand_value(value, inheritance_depth, structural_depth + 1, params)?
            } else {
                value.clone()
            };
            args.insert(expanded_key, expanded_value);
        }

        Ok(UseSpec { name, args })
    }
}

fn merge_objects(target: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        match (target.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                merge_objects(existing, incoming);
            }
            (_, value) => {
                target.insert(key, value);
            }
        }
    }
}
