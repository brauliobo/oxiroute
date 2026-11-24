use std::{
    collections::HashSet,
    fs,
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue, chunk::ChunkMode};
use serde_json::Value;

use crate::{
    MAX_DEPENDENCY_PATHS, MAX_SOURCE_BYTES, MAX_STRING_BYTES, limits::validate_value, toml, yaml,
};

const MAX_INCLUDE_DEPTH: usize = 16;
const MEMORY_ONLY_ERROR: &str = "filesystem loading is disabled in memory-only Lua evaluation";

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
    dependencies: LuaDependencies,
    include_state: Arc<Mutex<IncludeState>>,
}

struct ResolvedFile {
    canonical_path: PathBuf,
    rel_dir: PathBuf,
}

enum CandidateResolution {
    Found(ResolvedFile),
    NotFound,
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
    fn new(base_dir: &Path, dependencies: &LuaDependencies) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
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

    fn resolve_file(&self, rel_path: &str) -> mlua::Result<ResolvedFile> {
        let normalized = parse_relative_path(rel_path)?;
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
            if let CandidateResolution::Found(resolved) =
                resolve_candidate(&canonical_root, &local_candidate, rel_path)?
            {
                return Ok(resolved);
            }
        }

        match resolve_candidate(&canonical_root, &normalized, rel_path)? {
            CandidateResolution::Found(resolved) => Ok(resolved),
            CandidateResolution::NotFound => Err(mlua::Error::runtime(format!(
                "Lua loader file `{rel_path}` not found"
            ))),
        }
    }

    fn enter_include(
        &self,
        resolved: &ResolvedFile,
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

    fn read_file(&self, rel_path: &str, max_bytes: usize) -> mlua::Result<(ResolvedFile, String)> {
        let resolved = self.resolve_file(rel_path)?;
        self.dependencies.record(resolved.canonical_path.clone())?;
        let text = read_bounded_utf8(&resolved.canonical_path, rel_path, max_bytes)?;
        Ok((resolved, text))
    }
}

pub(super) fn register(
    lua: &Lua,
    base_dir: Option<&Path>,
    dependencies: &LuaDependencies,
) -> mlua::Result<()> {
    let globals = lua.globals();

    let Some(base_dir) = base_dir else {
        for name in [
            "include",
            "read_text",
            "read_lines",
            "read_json",
            "read_yaml",
            "read_toml",
        ] {
            let disabled = lua.create_function(|_, _rel_path: LuaValue| -> mlua::Result<LuaValue> {
                Err(mlua::Error::runtime(MEMORY_ONLY_ERROR))
            })?;
            globals.set(name, disabled)?;
        }
        return Ok(());
    };

    let ctx = LoaderContext::new(base_dir, dependencies);

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

    {
        let ctx = ctx.clone();
        let read_json_fn = lua.create_function(move |lua, rel_path: String| {
            let (_, text) = ctx.read_file(&rel_path, MAX_SOURCE_BYTES)?;
            let value: Value = serde_json::from_str(&text).map_err(|err| {
                mlua::Error::runtime(format!("invalid JSON in `{rel_path}`: {err}"))
            })?;
            validate_value(&value).map_err(|err| {
                mlua::Error::runtime(format!("invalid JSON in `{rel_path}`: {err}"))
            })?;
            lua.to_value(&value)
        })?;
        globals.set("read_json", read_json_fn)?;
    }

    {
        let ctx = ctx.clone();
        let read_yaml_fn = lua.create_function(move |lua, rel_path: String| {
            let (_, text) = ctx.read_file(&rel_path, MAX_SOURCE_BYTES)?;
            let value = yaml::decode(&text).map_err(|err| {
                mlua::Error::runtime(format!("invalid YAML in `{rel_path}`: {err}"))
            })?;
            lua.to_value(&value)
        })?;
        globals.set("read_yaml", read_yaml_fn)?;
    }

    {
        let read_toml_fn = lua.create_function(move |lua, rel_path: String| {
            let (_, text) = ctx.read_file(&rel_path, MAX_SOURCE_BYTES)?;
            let value = toml::decode(&text).map_err(|err| {
                mlua::Error::runtime(format!("invalid TOML in `{rel_path}`: {err}"))
            })?;
            lua.to_value(&value)
        })?;
        globals.set("read_toml", read_toml_fn)?;
    }

    Ok(())
}

fn parse_relative_path(rel_path: &str) -> mlua::Result<PathBuf> {
    if rel_path.is_empty() {
        return Err(mlua::Error::runtime("Lua loader path must not be empty"));
    }
    if rel_path.contains('\0') {
        return Err(mlua::Error::runtime(
            "Lua loader path must not contain NUL bytes",
        ));
    }
    let path = Path::new(rel_path);
    if path.is_absolute() {
        return Err(mlua::Error::runtime(format!(
            "Lua loader path `{rel_path}` must be relative"
        )));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(mlua::Error::runtime(format!(
                    "Lua loader path `{rel_path}` must not contain `..` traversal"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(mlua::Error::runtime(format!(
                    "Lua loader path `{rel_path}` must be relative"
                )));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(mlua::Error::runtime(format!(
            "Lua loader path `{rel_path}` must refer to a file"
        )));
    }

    Ok(normalized)
}

fn resolve_candidate(
    canonical_root: &Path,
    rel_candidate: &Path,
    display_path: &str,
) -> mlua::Result<CandidateResolution> {
    let mut components = rel_candidate.components().peekable();
    let mut cursor = canonical_root.to_path_buf();

    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(mlua::Error::runtime(format!(
                "Lua loader path `{display_path}` contains an invalid component"
            )));
        };
        cursor.push(segment);
        let is_last = components.peek().is_none();

        let meta = match fs::symlink_metadata(&cursor) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(CandidateResolution::NotFound);
            }
            Err(err) => {
                return Err(mlua::Error::runtime(format!(
                    "failed to inspect `{display_path}`: {err}"
                )));
            }
        };

        if meta.file_type().is_symlink() {
            return Err(mlua::Error::runtime(format!(
                "symlinks are not allowed in Lua loader path `{display_path}`"
            )));
        }

        if is_last {
            if !meta.file_type().is_file() {
                return Err(mlua::Error::runtime(format!(
                    "Lua loader path `{display_path}` must be a regular file"
                )));
            }
        } else if !meta.file_type().is_dir() {
            return Err(mlua::Error::runtime(format!(
                "Lua loader path `{display_path}` contains a non-directory component"
            )));
        }
    }

    let canonical_path = fs::canonicalize(&cursor).map_err(|err| {
        mlua::Error::runtime(format!("failed to canonicalize `{display_path}`: {err}"))
    })?;
    if canonical_path != cursor || !canonical_path.starts_with(canonical_root) {
        return Err(mlua::Error::runtime(format!(
            "Lua loader path `{display_path}` escapes the configuration directory"
        )));
    }

    let rel_dir = rel_candidate
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    Ok(CandidateResolution::Found(ResolvedFile {
        canonical_path,
        rel_dir,
    }))
}

fn read_bounded_utf8(path: &Path, display_path: &str, max_bytes: usize) -> mlua::Result<String> {
    let file = fs::File::open(path)
        .map_err(|err| mlua::Error::runtime(format!("failed to open `{display_path}`: {err}")))?;
    let meta = file.metadata().map_err(|err| {
        mlua::Error::runtime(format!("failed to inspect `{display_path}`: {err}"))
    })?;
    if !meta.is_file() {
        return Err(mlua::Error::runtime(format!(
            "Lua loader path `{display_path}` must be a regular file"
        )));
    }

    let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if meta.len() > max_bytes_u64 {
        return Err(mlua::Error::runtime(format!(
            "Lua loader file `{display_path}` exceeds the {max_bytes}-byte size limit"
        )));
    }

    let take_limit = max_bytes_u64.saturating_add(1);
    let mut bytes = Vec::new();
    file.take(take_limit)
        .read_to_end(&mut bytes)
        .map_err(|err| mlua::Error::runtime(format!("failed to read `{display_path}`: {err}")))?;

    if bytes.len() > max_bytes {
        return Err(mlua::Error::runtime(format!(
            "Lua loader file `{display_path}` exceeds the {max_bytes}-byte size limit"
        )));
    }

    String::from_utf8(bytes).map_err(|err| {
        mlua::Error::runtime(format!("Lua loader file `{display_path}` is not valid UTF-8: {err}"))
    })
}
