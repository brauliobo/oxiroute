use std::{fs, path::Path};

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_SOURCE_BYTES, SourceOverrides, resolve_jailed_target,
    resolve_source_with_format, resolve_source_with_overrides,
};
use serde::Serialize;

use super::{
    AuthoredRevision, CanonicalConfigCoordinator, CanonicalStorage, ConfigDraftRejection,
    ConfigLoadRejection, ConfigValidationOutcome, ConfigWriteBlock, EffectiveRevision,
    PersistableConfigCandidate, rejection, source_error_diagnostic,
};
use crate::atomic_file::write_atomic;

/// The exact authored text of the canonical file and the revision of its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredSource {
    pub text: String,
    pub revision: AuthoredRevision,
    pub format: ConfigFormat,
}

/// Metadata for one file in the directory-jailed configuration workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileEntry {
    /// Path relative to the configuration root directory (POSIX-style `/`).
    pub path: String,
    /// `"root"` for the canonical configuration file or `"include"` for an included file.
    pub kind: &'static str,
    /// Lowercase format name (`kdl`, `oxi`, `lua`, `yaml`, `toml`, `hocon`, `uci`, or `json`).
    pub format: String,
    /// SHA-256 revision of the exact file bytes on disk.
    pub revision: AuthoredRevision,
    /// File size in bytes.
    pub size_bytes: usize,
    /// Whether this file can be edited through the management API.
    pub writable: bool,
}

/// Raw UTF-8 content and metadata of one workspace file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFileContent {
    pub path: String,
    pub kind: &'static str,
    pub format: String,
    pub text: String,
    pub revision: AuthoredRevision,
    pub is_root: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceFileError {
    #[error("{0}")]
    InvalidPath(String),
    #[error("{0}")]
    NotFound(String),
    #[error("file exceeds the {MAX_SOURCE_BYTES}-byte limit")]
    TooLarge,
    #[error("file is not valid UTF-8")]
    InvalidUtf8,
    #[error("revision conflict")]
    Conflict {
        current_revision: Option<AuthoredRevision>,
    },
    #[error("{0}")]
    Io(String),
}

impl CanonicalConfigCoordinator {
    /// Reads the authored file without resolving it, so a file that no longer validates can still
    /// be shown and repaired.
    ///
    /// # Errors
    ///
    /// Returns the redacted rejection when the file cannot be read securely or is not UTF-8.
    pub fn read_source(&self) -> Result<AuthoredSource, ConfigLoadRejection> {
        let _operation = self.lock();
        let (bytes, revision) = self.read_disk()?;
        let text = String::from_utf8(bytes).map_err(|error| {
            rejection(
                Some(revision.clone()),
                source_error_diagnostic(&ConfigSourceError::Utf8(error.utf8_error())),
            )
        })?;
        Ok(AuthoredSource {
            text,
            revision,
            format: self.format,
        })
    }

    /// Resolves authored text exactly as the canonical file is resolved, without touching disk.
    ///
    /// Native references inside the text resolve against the directory of the canonical file.
    /// The candidate keeps the authored bytes, so saving it preserves comments, formatting and Lua
    /// logic.
    #[must_use]
    pub fn prepare_source(&self, text: &str, format: ConfigFormat) -> ConfigValidationOutcome {
        match resolve_source_with_format(&self.canonical_path, text.as_bytes(), format) {
            Ok(resolved) => ConfigValidationOutcome::Valid(Box::new(
                PersistableConfigCandidate::new(
                    EffectiveRevision::of(&resolved.canonical_kdl, &resolved.config),
                    resolved.config,
                    resolved.format,
                    resolved.compositional,
                    resolved.dependencies,
                    text.to_owned(),
                    Vec::new(),
                )
                .into_authored(),
            )),
            Err(error) => ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&error)],
            }),
        }
    }

    /// The revision of the exact bytes on disk, without resolving them.
    ///
    /// # Errors
    ///
    /// Returns the redacted rejection when the file cannot be read securely.
    pub fn disk_revision(&self) -> Result<AuthoredRevision, ConfigLoadRejection> {
        let _operation = self.lock();
        self.read_disk().map(|(_, revision)| revision)
    }

    /// What stops a save, when something does: a read-only mount or a directory the process may
    /// not write.
    #[must_use]
    pub fn write_block(&self) -> Option<ConfigWriteBlock> {
        CanonicalStorage::open(&self.canonical_path)
            .map_or(Some(ConfigWriteBlock::Unavailable), |storage| {
                storage.write_block()
            })
    }

    fn root_dir(&self) -> &Path {
        self.canonical_path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    fn root_file_name(&self) -> String {
        self.canonical_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("oxiroute.kdl")
            .to_owned()
    }

    /// Lists the canonical root file plus every directory-jailed included file discovered during
    /// source resolution.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigLoadRejection`] when the root file itself cannot be read from disk.
    pub fn list_workspace_files(&self) -> Result<Vec<WorkspaceFileEntry>, ConfigLoadRejection> {
        let _operation = self.lock();
        let (root_bytes, root_revision) = self.read_disk()?;
        let writable = self.write_block().is_none();
        let mut entries = vec![WorkspaceFileEntry {
            path: self.root_file_name(),
            kind: "root",
            format: self.format.wire_name().to_owned(),
            revision: root_revision,
            size_bytes: root_bytes.len(),
            writable,
        }];

        let Ok(canonical_root_dir) = fs::canonicalize(self.root_dir()) else {
            return Ok(entries);
        };
        let Ok(resolved) =
            resolve_source_with_format(&self.canonical_path, &root_bytes, self.format)
        else {
            return Ok(entries);
        };

        for included in resolved.included_files {
            let Ok(rel) = included.strip_prefix(&canonical_root_dir) else {
                continue;
            };
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if rel_str.is_empty() || rel_str == entries[0].path {
                continue;
            }
            let Ok(bytes) = fs::read(&included) else {
                continue;
            };
            if bytes.len() > MAX_SOURCE_BYTES {
                continue;
            }
            let format = file_format_label(&included);
            entries.push(WorkspaceFileEntry {
                path: rel_str,
                kind: "include",
                format,
                revision: AuthoredRevision::from_bytes(&bytes),
                size_bytes: bytes.len(),
                writable,
            });
        }
        Ok(entries)
    }

    /// Reads one directory-jailed file (either the root or an included file) as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceFileError`] for traversal/symlink rejection, missing file, size limit,
    /// or non-UTF-8 content.
    pub fn read_workspace_file(
        &self,
        rel_path: &str,
    ) -> Result<WorkspaceFileContent, WorkspaceFileError> {
        let _operation = self.lock();
        let (canonical_dir, target) = resolve_jailed_target(self.root_dir(), rel_path, true)
            .map_err(|msg| {
                if msg.contains("not found") {
                    WorkspaceFileError::NotFound(msg)
                } else {
                    WorkspaceFileError::InvalidPath(msg)
                }
            })?;
        let canonical_root =
            fs::canonicalize(&self.canonical_path).unwrap_or_else(|_| self.canonical_path.clone());
        let is_root = target == canonical_root;
        let bytes = fs::read(&target).map_err(|err| WorkspaceFileError::Io(err.to_string()))?;
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(WorkspaceFileError::TooLarge);
        }
        let text = String::from_utf8(bytes.clone()).map_err(|_| WorkspaceFileError::InvalidUtf8)?;
        let normalized_rel = target.strip_prefix(&canonical_dir).ok().map_or_else(
            || rel_path.to_owned(),
            |p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            },
        );
        Ok(WorkspaceFileContent {
            path: normalized_rel,
            kind: if is_root { "root" } else { "include" },
            format: if is_root {
                self.format.wire_name().to_owned()
            } else {
                file_format_label(&target)
            },
            text,
            revision: AuthoredRevision::from_bytes(&bytes),
            is_root,
        })
    }

    /// Validates a candidate edit to one directory-jailed workspace file in the context of the
    /// full configuration graph without modifying disk.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceFileError`] when the path is invalid or the root file cannot be read.
    pub fn prepare_workspace_file(
        &self,
        rel_path: &str,
        text: &str,
    ) -> Result<(ConfigValidationOutcome, Option<String>, bool), WorkspaceFileError> {
        if text.len() > MAX_SOURCE_BYTES {
            return Err(WorkspaceFileError::TooLarge);
        }
        let (_canonical_dir, target) = resolve_jailed_target(self.root_dir(), rel_path, false)
            .map_err(WorkspaceFileError::InvalidPath)?;
        let canonical_root =
            fs::canonicalize(&self.canonical_path).unwrap_or_else(|_| self.canonical_path.clone());
        if target == canonical_root {
            let disk_text = self.read_source().ok().map(|s| s.text);
            return Ok((self.prepare_source(text, self.format), disk_text, true));
        }

        let disk_text = fs::read(&target)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let root_source = self.read_source().map_err(|_| {
            WorkspaceFileError::Io("root configuration could not be read".to_owned())
        })?;
        let mut overrides = SourceOverrides::new();
        overrides.insert(target, text.as_bytes().to_vec());

        let outcome = match resolve_source_with_overrides(
            &self.canonical_path,
            root_source.text.as_bytes(),
            self.format,
            Some(&overrides),
        ) {
            Ok(resolved) => ConfigValidationOutcome::Valid(Box::new(
                PersistableConfigCandidate::new(
                    EffectiveRevision::of(&resolved.canonical_kdl, &resolved.config),
                    resolved.config,
                    resolved.format,
                    resolved.compositional,
                    resolved.dependencies,
                    root_source.text,
                    Vec::new(),
                )
                .into_authored(),
            )),
            Err(error) => ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&error)],
            }),
        };
        Ok((outcome, disk_text, false))
    }

    /// Atomically writes a directory-jailed included file after checking `expected` against the
    /// current bytes on disk.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceFileError`] on path violation, revision conflict, or I/O failure.
    pub fn save_workspace_file(
        &self,
        rel_path: &str,
        expected: Option<&AuthoredRevision>,
        text: &str,
    ) -> Result<AuthoredRevision, WorkspaceFileError> {
        if text.len() > MAX_SOURCE_BYTES {
            return Err(WorkspaceFileError::TooLarge);
        }
        let _operation = self.lock();
        let (_canonical_dir, target) = resolve_jailed_target(self.root_dir(), rel_path, false)
            .map_err(WorkspaceFileError::InvalidPath)?;
        let current = match fs::read(&target) {
            Ok(bytes) => Some(AuthoredRevision::from_bytes(&bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(WorkspaceFileError::Io(err.to_string())),
        };
        if current.as_ref() != expected {
            return Err(WorkspaceFileError::Conflict {
                current_revision: current,
            });
        }
        let mode = file_mode(&target).unwrap_or(0o600);
        write_atomic(&target, text.as_bytes(), mode)
            .map_err(|err| WorkspaceFileError::Io(err.to_string()))?;
        Ok(AuthoredRevision::from_bytes(text.as_bytes()))
    }
}

fn file_format_label(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => "json".to_owned(),
        Some(ext) => ConfigFormat::from_extension(ext)
            .map_or_else(|| ext.to_owned(), |fmt| fmt.wire_name().to_owned()),
        None => "text".to_owned(),
    }
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}
