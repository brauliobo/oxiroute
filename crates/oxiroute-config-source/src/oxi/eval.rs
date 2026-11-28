use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value};

use crate::{
    ConfigFormat, ConfigSourceError, MAX_DEPENDENCY_PATHS, MAX_SOURCE_BYTES, MAX_STRUCTURAL_DEPTH,
    SourceOverrides, hocon,
    jail::{
        JailCandidate, ReadFileError, ResolvedJailedFile, read_bounded_utf8, resolve_candidate,
        validate_relative_path,
    },
    kdl,
    limits::validate_value,
    lua, native, toml, uci, yaml,
};

use super::{Expr, ParsedDocument, RootItem, parse_err, parser};

const MAX_INCLUDE_DEPTH: usize = 16;

pub(super) fn evaluate(
    parsed: ParsedDocument,
    base_dir: Option<&Path>,
    overrides: Option<&SourceOverrides>,
) -> Result<(Value, Vec<PathBuf>, bool), ConfigSourceError> {
    let mut evaluator = Evaluator::new(base_dir, overrides);
    let root = evaluator.eval_document(parsed)?;
    let value = Value::Object(root);
    validate_value(&value)?;
    Ok((
        value,
        evaluator.dependencies,
        evaluator.compositional_syntax,
    ))
}

struct Evaluator<'a> {
    canonical_root: Option<PathBuf>,
    overrides: Option<&'a SourceOverrides>,
    active_files: Vec<PathBuf>,
    dir_stack: Vec<PathBuf>,
    dependencies: Vec<PathBuf>,
    seen_dependencies: HashSet<PathBuf>,
    vars: Map<String, Value>,
    compositional_syntax: bool,
}

impl<'a> Evaluator<'a> {
    fn new(base_dir: Option<&Path>, overrides: Option<&'a SourceOverrides>) -> Self {
        let canonical_root = base_dir.map(|dir| {
            let effective = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            fs::canonicalize(effective).unwrap_or_else(|_| effective.to_path_buf())
        });
        Self {
            canonical_root,
            overrides,
            active_files: Vec::new(),
            dir_stack: Vec::new(),
            dependencies: Vec::new(),
            seen_dependencies: HashSet::new(),
            vars: Map::new(),
            compositional_syntax: false,
        }
    }

    fn current_lua_base_dir(&self) -> Option<PathBuf> {
        let root = self.canonical_root.as_ref()?;
        match self.dir_stack.last() {
            Some(rel_dir) if !rel_dir.as_os_str().is_empty() => Some(root.join(rel_dir)),
            _ => Some(root.clone()),
        }
    }

    fn record_dependency(&mut self, path: PathBuf) -> Result<(), ConfigSourceError> {
        if self.seen_dependencies.insert(path.clone()) {
            if self.dependencies.len() >= MAX_DEPENDENCY_PATHS {
                return Err(ConfigSourceError::DependencyLimit);
            }
            self.dependencies.push(path);
        }
        Ok(())
    }

    fn eval_document(
        &mut self,
        parsed: ParsedDocument,
    ) -> Result<Map<String, Value>, ConfigSourceError> {
        let mut root = Map::new();
        let mut seen_entries = HashSet::new();
        let mut root_uses = Vec::new();
        let mut root_use_line = None;
        let mut deferred = Vec::with_capacity(parsed.items.len());

        // Evaluate top-level `vars` first so embedded Lua blocks receive `vars`.
        for item in parsed.items {
            if let RootItem::Entry {
                ref key,
                ref value,
                line,
            } = item
                && key == "vars"
            {
                if !seen_entries.insert(key.clone()) {
                    return Err(parse_err(line, "duplicate key `vars`"));
                }
                let evaluated = self.eval_expr(value.clone(), 1)?;
                if let Value::Object(ref map) = evaluated {
                    merge_objects_recursive(&mut self.vars, map.clone());
                }
                root.insert("vars".to_owned(), evaluated);
                continue;
            }
            deferred.push(item);
        }

        for item in deferred {
            self.apply_root_item(
                item,
                &mut root,
                &mut seen_entries,
                &mut root_uses,
                &mut root_use_line,
            )?;
        }

        if !root_uses.is_empty() {
            let line = root_use_line.unwrap_or(1);
            if !seen_entries.insert("use".to_owned()) {
                return Err(parse_err(line, "duplicate key `use`"));
            }
            let use_val = if root_uses.len() == 1 {
                root_uses.pop().unwrap_or(Value::Null)
            } else {
                Value::Array(root_uses)
            };
            root.insert("use".to_owned(), use_val);
        }
        Ok(root)
    }

    fn apply_root_item(
        &mut self,
        item: RootItem,
        root: &mut Map<String, Value>,
        seen_entries: &mut HashSet<String>,
        root_uses: &mut Vec<Value>,
        root_use_line: &mut Option<u32>,
    ) -> Result<(), ConfigSourceError> {
        match item {
            RootItem::Entry { key, value, line } => {
                if !seen_entries.insert(key.clone()) {
                    return Err(parse_err(line, format!("duplicate key `{key}`")));
                }
                let evaluated = self.eval_expr(value, 1)?;
                insert_root_entry(root, key, evaluated, line)
            }
            RootItem::Include { path, line } => {
                let included = self.eval_include(&path, line)?;
                if let Some(Value::Object(inc_vars)) = included.get("vars") {
                    merge_objects_recursive(&mut self.vars, inc_vars.clone());
                }
                merge_root_map(root, included, line)
            }
            RootItem::LuaBlock { code, line } => {
                self.compositional_syntax = true;
                let lua_val = self.eval_lua_chunk(&code)?;
                let Value::Object(map) = lua_val else {
                    return Err(parse_err(
                        line,
                        "top-level `lua` block must return an object",
                    ));
                };
                if let Some(Value::Object(lua_vars)) = map.get("vars") {
                    merge_objects_recursive(&mut self.vars, lua_vars.clone());
                }
                merge_root_map(root, map, line)
            }
            RootItem::Site { name, body, line } => {
                self.compositional_syntax = true;
                let body_val = self.eval_expr(body, 2)?;
                insert_named_map_entry(root, "sites", name, body_val, line)
            }
            RootItem::Template { name, body, line } => {
                self.compositional_syntax = true;
                let body_val = self.eval_expr(body, 2)?;
                insert_named_map_entry(root, "templates", name, body_val, line)
            }
            RootItem::NamedCollection {
                collection,
                name,
                body,
                line,
            } => {
                self.compositional_syntax = true;
                let mut map = match self.eval_expr(body, 2)? {
                    Value::Object(map) => map,
                    _ => Map::new(),
                };
                map.insert("name".to_owned(), Value::String(name));
                append_collection_item(root, collection, Value::Object(map), line)
            }
            RootItem::NativeServer { key, value, line } => {
                let evaluated = self.eval_expr(value, 1)?;
                insert_native_server(root, key, evaluated, line)
            }
            RootItem::Use { values, line } => {
                root_use_line.get_or_insert(line);
                for expr in values {
                    root_uses.push(self.eval_expr(expr, 1)?);
                }
                Ok(())
            }
        }
    }

    fn eval_expr(&mut self, expr: Expr, depth: usize) -> Result<Value, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(flag) => Ok(Value::Bool(flag)),
            Expr::Number(num) => Ok(Value::Number(num)),
            Expr::String(text) => Ok(Value::String(text)),
            Expr::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.eval_expr(item, depth + 1)?);
                }
                Ok(Value::Array(out))
            }
            Expr::Object(entries) => {
                let mut map = Map::with_capacity(entries.len());
                for (key, value, line) in entries {
                    let evaluated = self.eval_expr(value, depth + 1)?;
                    if map.insert(key.clone(), evaluated).is_some() {
                        return Err(parse_err(line, format!("duplicate key `{key}`")));
                    }
                }
                Ok(Value::Object(map))
            }
            Expr::LuaInline(code) => {
                self.compositional_syntax = true;
                self.eval_lua_chunk(&format!("return {code}"))
            }
            Expr::LuaBlock(code) => {
                self.compositional_syntax = true;
                self.eval_lua_chunk(&code)
            }
        }
    }

    fn eval_lua_chunk(&mut self, code: &str) -> Result<Value, ConfigSourceError> {
        let base_dir = self.current_lua_base_dir();
        let vars = (!self.vars.is_empty()).then_some(&self.vars);
        let (value, deps) = lua::eval_embedded(code, vars, base_dir.as_deref(), self.overrides)?;
        for dep in deps {
            self.record_dependency(dep)?;
        }
        Ok(value)
    }

    fn eval_include(
        &mut self,
        rel_path: &str,
        line: u32,
    ) -> Result<Map<String, Value>, ConfigSourceError> {
        let Some(root) = self.canonical_root.clone() else {
            return Err(parse_err(
                line,
                "include directives are disabled in memory-only decoding",
            ));
        };
        let canonical_root = fs::canonicalize(&root).map_err(|err| {
            parse_err(line, format!("failed to resolve OXI base directory: {err}"))
        })?;
        let normalized =
            validate_relative_path(rel_path, "include").map_err(|msg| parse_err(line, msg))?;
        let resolved = self.resolve_include_path(&canonical_root, &normalized, rel_path, line)?;

        if self.active_files.contains(&resolved.canonical_path) {
            return Err(parse_err(
                line,
                format!("include cycle detected for `{rel_path}`"),
            ));
        }
        if self.active_files.len() >= MAX_INCLUDE_DEPTH {
            return Err(parse_err(
                line,
                format!("include depth exceeds the {MAX_INCLUDE_DEPTH}-level limit"),
            ));
        }

        self.record_dependency(resolved.canonical_path.clone())?;
        let text = read_bounded_utf8(
            &resolved.canonical_path,
            rel_path,
            "include",
            MAX_SOURCE_BYTES,
            self.overrides,
        )
        .map_err(|err| match err {
            ReadFileError::TooLarge => ConfigSourceError::SourceTooLarge,
            ReadFileError::Utf8(e) => ConfigSourceError::Utf8(e),
            ReadFileError::Message(msg) => parse_err(line, msg),
        })?;
        let ext = resolved
            .canonical_path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);

        self.active_files.push(resolved.canonical_path.clone());
        self.dir_stack.push(resolved.rel_dir);
        let result = self.decode_included_file(
            ext.as_deref(),
            &resolved.canonical_path,
            &text,
            rel_path,
            line,
        );
        self.dir_stack.pop();
        self.active_files.pop();
        result
    }

    fn decode_included_file(
        &mut self,
        ext: Option<&str>,
        canonical_path: &Path,
        text: &str,
        rel_path: &str,
        line: u32,
    ) -> Result<Map<String, Value>, ConfigSourceError> {
        let value = match ext {
            Some("json") => {
                let val: Value = serde_json::from_str(text).map_err(|err| {
                    parse_err(line, format!("invalid JSON in `{rel_path}`: {err}"))
                })?;
                validate_value(&val)?;
                val
            }
            Some(other) => match ConfigFormat::from_extension(other) {
                Some(ConfigFormat::Oxi) => {
                    let parsed = parser::parse(text)?;
                    Value::Object(self.eval_document(parsed)?)
                }
                Some(ConfigFormat::Kdl) => kdl::decode(text)?,
                Some(ConfigFormat::Yaml) => yaml::decode(text)?,
                Some(ConfigFormat::Toml) => toml::decode(text)?,
                Some(ConfigFormat::Hocon) => hocon::decode(text)?,
                Some(ConfigFormat::Uci) => uci::decode(text)?,
                Some(ConfigFormat::Lua) => {
                    let file_dir = canonical_path.parent().unwrap_or_else(|| Path::new("."));
                    let (val, deps) = lua::decode_with_base_dir(text, file_dir, self.overrides)?;
                    for dep in deps {
                        self.record_dependency(dep)?;
                    }
                    val
                }
                None => {
                    return Err(parse_err(
                        line,
                        format!("unsupported include file extension for `{rel_path}`"),
                    ));
                }
            },
            None => {
                return Err(parse_err(
                    line,
                    format!("unsupported include file extension for `{rel_path}`"),
                ));
            }
        };

        match value {
            Value::Object(map) => Ok(map),
            _ => Err(parse_err(
                line,
                format!("included file `{rel_path}` must decode to an object"),
            )),
        }
    }

    fn resolve_include_path(
        &self,
        canonical_root: &Path,
        normalized: &Path,
        display_path: &str,
        line: u32,
    ) -> Result<ResolvedJailedFile, ConfigSourceError> {
        if let Some(current_rel_dir) = self.dir_stack.last()
            && !current_rel_dir.as_os_str().is_empty()
        {
            let local_candidate = current_rel_dir.join(normalized);
            if let JailCandidate::Found(resolved) = resolve_candidate(
                canonical_root,
                &local_candidate,
                display_path,
                "include",
                self.overrides,
            )
            .map_err(|msg| parse_err(line, msg))?
            {
                return Ok(resolved);
            }
        }
        match resolve_candidate(
            canonical_root,
            normalized,
            display_path,
            "include",
            self.overrides,
        )
        .map_err(|msg| parse_err(line, msg))?
        {
            JailCandidate::Found(resolved) => Ok(resolved),
            JailCandidate::NotFound => Err(parse_err(
                line,
                format!("included file `{display_path}` not found"),
            )),
        }
    }
}

fn insert_root_entry(
    root: &mut Map<String, Value>,
    key: String,
    value: Value,
    line: u32,
) -> Result<(), ConfigSourceError> {
    if let Some(existing) = root.get_mut(&key) {
        match (existing, value) {
            (Value::Object(target), Value::Object(overlay)) => {
                merge_objects_recursive(target, overlay);
                Ok(())
            }
            (Value::Array(target), Value::Array(mut overlay)) => {
                target.append(&mut overlay);
                Ok(())
            }
            _ => Err(parse_err(line, format!("duplicate key `{key}`"))),
        }
    } else {
        root.insert(key, value);
        Ok(())
    }
}

fn insert_named_map_entry(
    root: &mut Map<String, Value>,
    container_key: &'static str,
    name: String,
    value: Value,
    line: u32,
) -> Result<(), ConfigSourceError> {
    let entry = root
        .entry(container_key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(map) = entry else {
        return Err(parse_err(
            line,
            format!("`{container_key}` must be an object"),
        ));
    };
    if map.contains_key(&name) {
        return Err(parse_err(
            line,
            format!("duplicate `{container_key}` entry `{name}`"),
        ));
    }
    map.insert(name, value);
    Ok(())
}

fn append_collection_item(
    root: &mut Map<String, Value>,
    collection: &'static str,
    item: Value,
    line: u32,
) -> Result<(), ConfigSourceError> {
    let entry = root
        .entry(collection.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(array) = entry else {
        return Err(parse_err(line, format!("`{collection}` must be an array")));
    };
    array.push(item);
    Ok(())
}

fn insert_native_server(
    root: &mut Map<String, Value>,
    key: &'static str,
    value: Value,
    line: u32,
) -> Result<(), ConfigSourceError> {
    match root.remove(key) {
        None => {
            root.insert(key.to_owned(), value);
            Ok(())
        }
        Some(Value::Array(mut list)) => {
            list.push(value);
            root.insert(key.to_owned(), Value::Array(list));
            Ok(())
        }
        Some(existing @ Value::Object(_)) => {
            root.insert(key.to_owned(), Value::Array(vec![existing, value]));
            Ok(())
        }
        Some(_) => Err(parse_err(
            line,
            format!("`{key}` must be an object or array of objects"),
        )),
    }
}

fn is_native_server_key(key: &str) -> bool {
    native::is_native_server_key(key)
}

fn merge_root_map(
    target: &mut Map<String, Value>,
    overlay: Map<String, Value>,
    line: u32,
) -> Result<(), ConfigSourceError> {
    for (key, value) in overlay {
        if is_native_server_key(&key) && target.contains_key(&key) {
            let existing = target.remove(&key).unwrap_or(Value::Null);
            let combined = match (existing, value) {
                (Value::Array(mut a), Value::Array(mut b)) => {
                    a.append(&mut b);
                    Value::Array(a)
                }
                (Value::Array(mut a), obj @ Value::Object(_)) => {
                    a.push(obj);
                    Value::Array(a)
                }
                (obj @ Value::Object(_), Value::Array(mut b)) => {
                    let mut out = Vec::with_capacity(b.len() + 1);
                    out.push(obj);
                    out.append(&mut b);
                    Value::Array(out)
                }
                (a @ Value::Object(_), b @ Value::Object(_)) => Value::Array(vec![a, b]),
                _ => {
                    return Err(parse_err(
                        line,
                        format!("cannot merge conflicting `{key}` values"),
                    ));
                }
            };
            target.insert(key, combined);
            continue;
        }
        if let Some(existing) = target.get_mut(&key) {
            match (existing, value) {
                (Value::Object(dst), Value::Object(src)) => merge_objects_recursive(dst, src),
                (Value::Array(dst), Value::Array(mut src)) => dst.append(&mut src),
                (dst, new_val) => *dst = new_val,
            }
        } else {
            target.insert(key, value);
        }
    }
    Ok(())
}

fn merge_objects_recursive(target: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        if let (Some(Value::Object(dst)), Value::Object(src)) = (target.get_mut(&key), &value) {
            merge_objects_recursive(dst, src.clone());
        } else {
            target.insert(key, value);
        }
    }
}
