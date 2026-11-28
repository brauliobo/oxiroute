use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
};

use crate::MAX_SOURCE_BYTES;

/// Optional in-memory file overrides keyed by canonicalized absolute path.
///
/// Used when validating a candidate edit to an included child file in the context of the full
/// configuration graph before touching disk.
pub type SourceOverrides = BTreeMap<PathBuf, Vec<u8>>;

#[derive(Clone, Debug)]
pub(crate) struct ResolvedJailedFile {
    pub(crate) canonical_path: PathBuf,
    pub(crate) rel_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) enum JailCandidate {
    Found(ResolvedJailedFile),
    NotFound,
}

/// Validates and normalizes a relative path for directory-jailed configuration files.
///
/// Rejects empty paths, NUL bytes, absolute paths, prefixes, and `..` traversal components.
///
/// # Errors
///
/// Returns a human-readable error message describing why the path was rejected.
pub fn validate_relative_path(rel_path: &str, context: &str) -> Result<PathBuf, String> {
    if rel_path.is_empty() {
        return Err(format!("{context} path must not be empty"));
    }
    if rel_path.contains('\0') {
        return Err(format!("{context} path must not contain NUL bytes"));
    }
    let path = Path::new(rel_path);
    if path.is_absolute() {
        return Err(format!("{context} path `{rel_path}` must be relative"));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "{context} path `{rel_path}` must not contain `..` traversal"
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("{context} path `{rel_path}` must be relative"));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("{context} path `{rel_path}` must refer to a file"));
    }
    Ok(normalized)
}

/// Resolves a candidate relative path inside `canonical_root` without following any symlink.
///
/// When `overrides` contains a matching canonical path inside `canonical_root` (even for a
/// not-yet-created file whose existing parent directories are real non-symlink directories),
/// it resolves as `Found`.
pub(crate) fn resolve_candidate(
    canonical_root: &Path,
    rel_candidate: &Path,
    display_path: &str,
    context: &str,
    overrides: Option<&SourceOverrides>,
) -> Result<JailCandidate, String> {
    let mut components = rel_candidate.components().peekable();
    let mut cursor = canonical_root.to_path_buf();

    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(format!(
                "{context} path `{display_path}` contains an invalid component"
            ));
        };
        cursor.push(segment);
        let is_last = components.peek().is_none();

        let meta = match fs::symlink_metadata(&cursor) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if is_last
                    && overrides.is_some_and(|map| map.contains_key(&cursor))
                    && cursor.starts_with(canonical_root)
                {
                    return Ok(JailCandidate::Found(ResolvedJailedFile {
                        canonical_path: cursor,
                        rel_dir: rel_candidate
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_default(),
                    }));
                }
                return Ok(JailCandidate::NotFound);
            }
            Err(err) => {
                return Err(format!("failed to inspect `{display_path}`: {err}"));
            }
        };
        if meta.file_type().is_symlink() {
            return Err(format!(
                "symlinks are not allowed in {context} path `{display_path}`"
            ));
        }
        if is_last && !meta.file_type().is_file() {
            return Err(format!(
                "{context} path `{display_path}` must be a regular file"
            ));
        }
        if !is_last && !meta.file_type().is_dir() {
            return Err(format!(
                "{context} path `{display_path}` contains a non-directory component"
            ));
        }
    }

    let canonical_path = fs::canonicalize(&cursor)
        .map_err(|err| format!("failed to canonicalize `{display_path}`: {err}"))?;
    if canonical_path != cursor || !canonical_path.starts_with(canonical_root) {
        return Err(format!(
            "{context} path `{display_path}` escapes the configuration directory"
        ));
    }

    Ok(JailCandidate::Found(ResolvedJailedFile {
        canonical_path,
        rel_dir: rel_candidate
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
    }))
}

#[derive(Debug)]
pub(crate) enum ReadFileError {
    TooLarge,
    Utf8(std::str::Utf8Error),
    Message(String),
}

/// Reads a regular file (or its in-memory override) as bounded UTF-8.
pub(crate) fn read_bounded_utf8(
    path: &Path,
    display_path: &str,
    context: &str,
    max_bytes: usize,
    overrides: Option<&SourceOverrides>,
) -> Result<String, ReadFileError> {
    if let Some(bytes) = overrides.and_then(|map| map.get(path)) {
        if bytes.len() > max_bytes {
            return Err(ReadFileError::TooLarge);
        }
        return std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(ReadFileError::Utf8);
    }

    let file = fs::File::open(path)
        .map_err(|err| ReadFileError::Message(format!("failed to open `{display_path}`: {err}")))?;
    let meta = file.metadata().map_err(|err| {
        ReadFileError::Message(format!("failed to inspect `{display_path}`: {err}"))
    })?;
    if !meta.is_file() {
        return Err(ReadFileError::Message(format!(
            "{context} path `{display_path}` must be a regular file"
        )));
    }

    let max_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if meta.len() > max_u64 {
        return Err(ReadFileError::TooLarge);
    }

    let mut bytes = Vec::new();
    file.take(max_u64.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| ReadFileError::Message(format!("failed to read `{display_path}`: {err}")))?;
    if bytes.len() > max_bytes {
        return Err(ReadFileError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|err| ReadFileError::Utf8(err.utf8_error()))
}

/// Validates and canonicalizes an arbitrary relative file path within `base_dir` for safe reading
/// or writing (rejecting symlinks and directory escapes). Returns `(canonical_root, target_path)`.
///
/// # Errors
///
/// Returns a descriptive error string when the base directory cannot be resolved, the path is
/// invalid, or any existing path component is a symlink or escapes `base_dir`.
pub fn resolve_jailed_target(
    base_dir: &Path,
    rel_path: &str,
    must_exist: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let normalized = validate_relative_path(rel_path, "config")?;
    let effective_root = if base_dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base_dir
    };
    let canonical_root = fs::canonicalize(effective_root)
        .map_err(|err| format!("failed to resolve configuration directory: {err}"))?;

    let mut components = normalized.components().peekable();
    let mut cursor = canonical_root.clone();

    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(format!(
                "config path `{rel_path}` contains an invalid component"
            ));
        };
        cursor.push(segment);
        let is_last = components.peek().is_none();

        match fs::symlink_metadata(&cursor) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(format!(
                        "symlinks are not allowed in config path `{rel_path}`"
                    ));
                }
                if is_last && !meta.file_type().is_file() {
                    return Err(format!("config path `{rel_path}` must be a regular file"));
                }
                if !is_last && !meta.file_type().is_dir() {
                    return Err(format!(
                        "config path `{rel_path}` contains a non-directory component"
                    ));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if must_exist {
                    return Err(format!("config file `{rel_path}` not found"));
                }
                if !is_last {
                    return Err(format!("parent directory for `{rel_path}` does not exist"));
                }
            }
            Err(err) => {
                return Err(format!("failed to inspect `{rel_path}`: {err}"));
            }
        }
    }

    if !cursor.starts_with(&canonical_root) {
        return Err(format!(
            "config path `{rel_path}` escapes the configuration directory"
        ));
    }
    if must_exist {
        let canonical_target = fs::canonicalize(&cursor)
            .map_err(|err| format!("failed to canonicalize `{rel_path}`: {err}"))?;
        if canonical_target != cursor || !canonical_target.starts_with(&canonical_root) {
            return Err(format!(
                "config path `{rel_path}` escapes the configuration directory"
            ));
        }
        return Ok((canonical_root, canonical_target));
    }

    let _ = MAX_SOURCE_BYTES;
    Ok((canonical_root, cursor))
}
