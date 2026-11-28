use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue, chunk::ChunkMode};
use serde_json::Value;

use crate::{
    MAX_DEPENDENCY_PATHS, MAX_SOURCE_BYTES, MAX_STRING_BYTES, SourceOverrides, hocon,
    jail::{
        JailCandidate, ReadFileError, ResolvedJailedFile, read_bounded_utf8, resolve_candidate,
        validate_relative_path,
    },
    kdl,
    limits::validate_value,
    oxi, toml, uci, yaml,
};

const MAX_INCLUDE_DEPTH: usize = 16;
const MEMORY_ONLY_ERROR: &str = "filesystem loading is disabled in memory-only Lua evaluation";

const LOADER_FUNCTIONS: [&str; 10] = [
    "include",
    "read_text",
    "read_lines",
    "read_json",
    "read_yaml",
    "read_toml",
    "read_kdl",
    "read_hocon",
    "read_uci",
    "read_oxi",
];

#[derive(Default)]
struct DependencyState {
    paths: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
}

#[derive(Clone, Default)]
pub(super) struct LuaDependencies {
    state: Arc<Mutex<DependencyState>>,
}

impl LuaDependencies {
    pub(super) fn into_paths(self) -> Vec<PathBuf> {
        self.state
            .lock()
            .map(|guard| guard.paths.clone())
            .unwrap_or_default()
    }

    fn record(&self, path: PathBuf) -> mlua::Result<()> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("Lua dependency tracker lock poisoned"))?;
        if guard.seen.insert(path.clone()) {
            if guard.paths.len() >= MAX_DEPENDENCY_PATHS {
                return Err(mlua::Error::runtime(format!(
                    "Lua dependency count exceeds the {MAX_DEPENDENCY_PATHS}-path limit"
                )));
            }
            guard.paths.push(path);
        }
        Ok(())
    }
}

#[derive(Default)]
struct IncludeState {
    active_files: Vec<PathBuf>,
    dir_stack: Vec<PathBuf>,
}

#[derive(Clone)]
struct LoaderContext {
    base_dir: PathBuf,
    overrides: Option<Arc<SourceOverrides>>,
    dependencies: LuaDependencies,
    include_state: Arc<Mutex<IncludeState>>,
}

struct IncludeGuard {
    state: Arc<Mutex<IncludeState>>,
}

impl Drop for IncludeGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.active_files.pop();
            guard.dir_stack.pop();
        }
    }
}

impl LoaderContext {
    fn new(
        base_dir: &Path,
        overrides: Option<&SourceOverrides>,
        dependencies: &LuaDependencies,
    ) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
            overrides: overrides.cloned().map(Arc::new),
            dependencies: dependencies.clone(),
            include_state: Arc::new(Mutex::new(IncludeState::default())),
        }
    }

    fn current_rel_dir(&self) -> mlua::Result<PathBuf> {
        let guard = self
            .include_state
            .lock()
            .map_err(|_| mlua::Error::runtime("Lua include state lock poisoned"))?;
        Ok(guard.dir_stack.last().cloned().unwrap_or_default())
    }

    fn resolve_file(&self, rel_path: &str) -> mlua::Result<ResolvedJailedFile> {
        let normalized =
            validate_relative_path(rel_path, "Lua loader").map_err(mlua::Error::runtime)?;
        let effective_root = if self.base_dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            self.base_dir.as_path()
        };
        let canonical_root = fs::canonicalize(effective_root).map_err(|err| {
            mlua::Error::runtime(format!("failed to resolve Lua base directory: {err}"))
        })?;

        let current_rel_dir = self.current_rel_dir()?;
        if !current_rel_dir.as_os_str().is_empty() {
            let local_candidate = current_rel_dir.join(&normalized);
            if let JailCandidate::Found(resolved) = resolve_candidate(
                &canonical_root,
                &local_candidate,
                rel_path,
                "Lua loader",
                self.overrides.as_deref(),
            )
            .map_err(mlua::Error::runtime)?
            {
                return Ok(resolved);
            }
        }

        match resolve_candidate(
            &canonical_root,
            &normalized,
            rel_path,
            "Lua loader",
            self.overrides.as_deref(),
        )
        .map_err(mlua::Error::runtime)?
        {
            JailCandidate::Found(resolved) => Ok(resolved),
            JailCandidate::NotFound => Err(mlua::Error::runtime(format!(
                "Lua loader file `{rel_path}` not found"
            ))),
        }
    }

    fn enter_include(
        &self,
        resolved: &ResolvedJailedFile,
        display_path: &str,
    ) -> mlua::Result<IncludeGuard> {
        let mut guard = self
            .include_state
            .lock()
            .map_err(|_| mlua::Error::runtime("Lua include state lock poisoned"))?;
        if guard.active_files.contains(&resolved.canonical_path) {
            return Err(mlua::Error::runtime(format!(
                "Lua include cycle detected for `{display_path}`"
            )));
        }
        if guard.active_files.len() >= MAX_INCLUDE_DEPTH {
            return Err(mlua::Error::runtime(format!(
                "Lua include depth exceeds the {MAX_INCLUDE_DEPTH}-level limit"
            )));
        }
        guard.active_files.push(resolved.canonical_path.clone());
        guard.dir_stack.push(resolved.rel_dir.clone());
        drop(guard);
        Ok(IncludeGuard {
            state: Arc::clone(&self.include_state),
        })
    }

    fn read_file(
        &self,
        rel_path: &str,
        max_bytes: usize,
    ) -> mlua::Result<(ResolvedJailedFile, String)> {
        let resolved = self.resolve_file(rel_path)?;
        self.dependencies.record(resolved.canonical_path.clone())?;
        let text = read_bounded_utf8(
            &resolved.canonical_path,
            rel_path,
            "Lua loader",
            max_bytes,
            self.overrides.as_deref(),
        )
        .map_err(|err| match err {
            ReadFileError::TooLarge => mlua::Error::runtime(format!(
                "Lua loader file `{rel_path}` exceeds the {max_bytes}-byte size limit"
            )),
            ReadFileError::Utf8(e) => mlua::Error::runtime(format!(
                "Lua loader file `{rel_path}` is not valid UTF-8: {e}"
            )),
            ReadFileError::Message(msg) => mlua::Error::runtime(msg),
        })?;
        Ok((resolved, text))
    }
}

pub(super) fn register(
    lua: &Lua,
    base_dir: Option<&Path>,
    overrides: Option<&SourceOverrides>,
    dependencies: &LuaDependencies,
) -> mlua::Result<()> {
    let globals = lua.globals();

    let Some(base_dir) = base_dir else {
        for name in LOADER_FUNCTIONS {
            let disabled =
                lua.create_function(|_, _rel_path: LuaValue| -> mlua::Result<LuaValue> {
                    Err(mlua::Error::runtime(MEMORY_ONLY_ERROR))
                })?;
            globals.set(name, disabled)?;
        }
        return Ok(());
    };

    let ctx = LoaderContext::new(base_dir, overrides, dependencies);

    {
        let ctx = ctx.clone();
        let include_fn = lua.create_function(move |lua, rel_path: String| {
            let (resolved, source) = ctx.read_file(&rel_path, MAX_SOURCE_BYTES)?;
            let _guard = ctx.enter_include(&resolved, &rel_path)?;
            lua.load(&source)
                .set_name(&rel_path)
                .set_mode(ChunkMode::Text)
                .eval::<LuaValue>()
        })?;
        globals.set("include", include_fn)?;
    }

    {
        let ctx = ctx.clone();
        let read_text_fn = lua.create_function(move |_, rel_path: String| {
            let (_, text) = ctx.read_file(&rel_path, MAX_STRING_BYTES)?;
            Ok(text)
        })?;
        globals.set("read_text", read_text_fn)?;
    }

    {
        let ctx = ctx.clone();
        let read_lines_fn = lua.create_function(move |lua, rel_path: String| {
            let (_, text) = ctx.read_file(&rel_path, MAX_STRING_BYTES)?;
            let table: Table = lua.create_table()?;
            table.set_metatable(Some(lua.array_metatable()))?;
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                table.push(trimmed)?;
            }
            Ok(table)
        })?;
        globals.set("read_lines", read_lines_fn)?;
    }

    register_format_reader(lua, &globals, &ctx, "read_json", "JSON", |text| {
        let value: Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
        validate_value(&value).map_err(|err| err.to_string())?;
        Ok(value)
    })?;
    register_format_reader(lua, &globals, &ctx, "read_yaml", "YAML", |text| {
        yaml::decode(text).map_err(|err| err.to_string())
    })?;
    register_format_reader(lua, &globals, &ctx, "read_toml", "TOML", |text| {
        toml::decode(text).map_err(|err| err.to_string())
    })?;
    register_format_reader(lua, &globals, &ctx, "read_kdl", "KDL 2", |text| {
        kdl::decode(text).map_err(|err| err.to_string())
    })?;
    register_format_reader(lua, &globals, &ctx, "read_hocon", "HOCON", |text| {
        hocon::decode(text).map_err(|err| err.to_string())
    })?;
    register_format_reader(lua, &globals, &ctx, "read_uci", "UCI", |text| {
        uci::decode(text).map_err(|err| err.to_string())
    })?;

    {
        let ctx = ctx.clone();
        let read_oxi_fn = lua.create_function(move |lua, rel_path: String| {
            let (resolved, text) = ctx.read_file(&rel_path, MAX_SOURCE_BYTES)?;
            let file_dir = resolved
                .canonical_path
                .parent()
                .unwrap_or_else(|| Path::new("."));
            let (value, deps, _) =
                oxi::decode_with_base_dir(&text, file_dir, ctx.overrides.as_deref()).map_err(
                    |err| mlua::Error::runtime(format!("invalid OXI in `{rel_path}`: {err}")),
                )?;
            for dep in deps {
                ctx.dependencies.record(dep)?;
            }
            lua.to_value(&value)
        })?;
        globals.set("read_oxi", read_oxi_fn)?;
    }

    Ok(())
}

fn register_format_reader(
    lua: &Lua,
    globals: &Table,
    ctx: &LoaderContext,
    fn_name: &str,
    label: &'static str,
    decode: fn(&str) -> Result<Value, String>,
) -> mlua::Result<()> {
    let ctx = ctx.clone();
    let reader = lua.create_function(move |lua, rel_path: String| {
        let (_, text) = ctx.read_file(&rel_path, MAX_SOURCE_BYTES)?;
        let value = decode(&text).map_err(|err| {
            mlua::Error::runtime(format!("invalid {label} in `{rel_path}`: {err}"))
        })?;
        lua.to_value(&value)
    })?;
    globals.set(fn_name, reader)
}
