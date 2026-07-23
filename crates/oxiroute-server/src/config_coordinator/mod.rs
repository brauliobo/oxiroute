use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use openssl::sha::sha256;
use oxiroute_config::{Config, ConfigError, load_lua, render_lua, validate_config};
use serde::Serialize;

mod storage;

use storage::{CanonicalStorage, ReplaceControl, ReplaceResult};

/// The canonical file is bounded to the same one-MiB limit as the restricted Lua loader.
pub const MAX_CANONICAL_CONFIG_BYTES: usize = 1024 * 1024;

/// A SHA-256 revision of the exact bytes stored in, or proposed for, the canonical file.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ConfigRevision(String);

impl ConfigRevision {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(lower_hex(&sha256(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConfigRevision")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ConfigRevision {
    type Err = ConfigRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ConfigRevisionParseError);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("configuration revision must be a 64-character SHA-256 hexadecimal value")]
pub struct ConfigRevisionParseError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDiagnosticStage {
    Read,
    Parse,
    Validation,
    Render,
    Conflict,
    Write,
    Sync,
    Rollback,
}

/// A deliberately redacted diagnostic suitable for returning across a management boundary.
///
/// Diagnostics never retain source text, configured paths, OS error strings, or rejected values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigDiagnostic {
    pub code: &'static str,
    pub severity: ConfigDiagnosticSeverity,
    pub stage: ConfigDiagnosticStage,
    pub message: &'static str,
}

/// A valid canonical-file view. `disk_revision` hashes the bytes read from disk, while
/// `candidate_revision` hashes the normalized deterministic preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CanonicalConfigDocument {
    pub disk_revision: ConfigRevision,
    pub candidate_revision: ConfigRevision,
    pub normalized_config: Config,
    pub lua_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLoadOutcome {
    Loaded(Box<CanonicalConfigDocument>),
    Rejected(ConfigLoadRejection),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigLoadRejection {
    pub disk_revision: Option<ConfigRevision>,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

/// A normalized typed draft and the exact Lua bytes that would be saved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValidatedConfigDraft {
    pub candidate_revision: ConfigRevision,
    pub normalized_config: Config,
    pub lua_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValidationOutcome {
    Valid(Box<ValidatedConfigDraft>),
    Invalid(ConfigDraftRejection),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigDraftRejection {
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSaveOutcome {
    /// The candidate revision is committed. Diagnostics may contain redacted cleanup warnings.
    Saved(CanonicalConfigDocument),
    Conflict(ConfigConflict),
    InvalidDraft(ConfigDraftRejection),
    Failed(ConfigSaveFailure),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigConflict {
    pub expected_revision: ConfigRevision,
    pub disk_revision: ConfigRevision,
    pub candidate_revision: ConfigRevision,
    pub normalized_config: Config,
    pub lua_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigSaveFailure {
    pub disk_revision: Option<ConfigRevision>,
    pub candidate_revision: ConfigRevision,
    pub normalized_config: Config,
    pub lua_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigCoordinatorPathError {
    #[error("canonical configuration path must identify one file")]
    MissingFileName,
}

/// Coordinates reads, validation, previews, and revision-checked durable saves for one canonical
/// Lua path. Runtime activation and its revision remain the caller's responsibility.
#[derive(Clone)]
pub struct CanonicalConfigCoordinator {
    canonical_path: PathBuf,
    operation_lock: Arc<Mutex<()>>,
}

impl CanonicalConfigCoordinator {
    /// Creates a coordinator without opening or creating the configured path.
    ///
    /// # Errors
    ///
    /// Returns an error when `canonical_path` does not contain a normal file-name component.
    pub fn new(canonical_path: impl Into<PathBuf>) -> Result<Self, ConfigCoordinatorPathError> {
        let canonical_path = canonical_path.into();
        storage::validate_path(&canonical_path)?;
        Ok(Self {
            canonical_path,
            operation_lock: Arc::new(Mutex::new(())),
        })
    }

    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Reads one stable no-follow snapshot and returns its normalized typed representation.
    #[must_use]
    pub fn load(&self) -> ConfigLoadOutcome {
        let _operation = self
            .operation_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let storage = match CanonicalStorage::open(&self.canonical_path) {
            Ok(storage) => storage,
            Err(error) => return rejected_load(None, error.diagnostic()),
        };
        let disk = match storage.read() {
            Ok(disk) => disk,
            Err(error) => return rejected_load(None, error.diagnostic()),
        };
        let disk_revision = ConfigRevision::from_bytes(&disk);
        let Ok(source) = std::str::from_utf8(&disk) else {
            return rejected_load(
                Some(disk_revision),
                diagnostic(
                    "E_SYNTAX",
                    ConfigDiagnosticStage::Parse,
                    "canonical configuration is not valid UTF-8",
                ),
            );
        };
        let normalized_config = match load_lua(source) {
            Ok(config) => config,
            Err(error) => {
                return rejected_load(Some(disk_revision), config_error_diagnostic(&error));
            }
        };
        let lua_preview = match render_lua(&normalized_config) {
            Ok(preview) => preview,
            Err(error) => {
                return rejected_load(Some(disk_revision), render_error_diagnostic(&error));
            }
        };

        ConfigLoadOutcome::Loaded(Box::new(CanonicalConfigDocument {
            disk_revision,
            candidate_revision: ConfigRevision::from_bytes(lua_preview.as_bytes()),
            normalized_config,
            lua_preview,
            diagnostics: Vec::new(),
        }))
    }

    /// Validates and normalizes a typed draft without reading or writing the canonical file.
    #[must_use]
    pub fn validate(&self, draft: Config) -> ConfigValidationOutcome {
        validate_draft(draft)
    }

    /// Saves a valid typed draft only when the exact on-disk bytes still match `expected_revision`.
    ///
    /// Independent coordinators and processes serialize through a pinned per-path filesystem lock.
    /// The candidate is written with mode `0600`, synced, atomically exchanged with the canonical
    /// name, checked against the expected revision at the exchange point, and followed by a parent
    /// directory sync. Once that commit sync succeeds, cleanup-sync degradation is returned as a
    /// warning in the saved document rather than as an unwritten failure. This method does not
    /// activate or publish a runtime generation.
    #[must_use]
    pub fn save(&self, expected_revision: &ConfigRevision, draft: Config) -> ConfigSaveOutcome {
        self.save_inner(
            expected_revision,
            draft,
            || Ok(()),
            || {},
            ReplaceControl::default(),
        )
    }

    fn save_inner<F, G>(
        &self,
        expected_revision: &ConfigRevision,
        draft: Config,
        before_exchange: F,
        after_exchange: G,
        control: ReplaceControl,
    ) -> ConfigSaveOutcome
    where
        F: FnOnce() -> Result<(), ()>,
        G: FnOnce(),
    {
        let candidate = match validate_draft(draft) {
            ConfigValidationOutcome::Valid(candidate) => *candidate,
            ConfigValidationOutcome::Invalid(rejection) => {
                return ConfigSaveOutcome::InvalidDraft(rejection);
            }
        };
        let _operation = self
            .operation_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let storage = match CanonicalStorage::open(&self.canonical_path) {
            Ok(storage) => storage,
            Err(error) => return failed_save(None, candidate, error.diagnostic()),
        };
        let transaction = match storage.lock_transaction() {
            Ok(transaction) => transaction,
            Err(error) => return failed_save(None, candidate, error.diagnostic()),
        };
        let initial_disk = match storage.read() {
            Ok(disk) => disk,
            Err(error) => return failed_save(None, candidate, error.diagnostic()),
        };
        let initial_revision = ConfigRevision::from_bytes(&initial_disk);
        if initial_revision != *expected_revision {
            return conflict(expected_revision, initial_revision, candidate);
        }

        match storage.replace(
            &transaction,
            expected_revision,
            candidate.lua_preview.as_bytes(),
            before_exchange,
            after_exchange,
            control,
        ) {
            Ok(ReplaceResult::Saved { cleanup_degraded }) => {
                let ValidatedConfigDraft {
                    candidate_revision,
                    normalized_config,
                    lua_preview,
                    ..
                } = candidate;
                ConfigSaveOutcome::Saved(CanonicalConfigDocument {
                    disk_revision: candidate_revision.clone(),
                    candidate_revision,
                    normalized_config,
                    lua_preview,
                    diagnostics: if cleanup_degraded {
                        vec![cleanup_durability_warning()]
                    } else {
                        Vec::new()
                    },
                })
            }
            Ok(ReplaceResult::Conflict(disk_revision)) => {
                conflict(expected_revision, disk_revision, candidate)
            }
            Err(error) => {
                let disk_revision = storage
                    .read()
                    .ok()
                    .map(|disk| ConfigRevision::from_bytes(&disk));
                failed_save(disk_revision, candidate, error.diagnostic())
            }
        }
    }
}

impl fmt::Debug for CanonicalConfigCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalConfigCoordinator")
            .field("canonical_path", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn validate_draft(mut draft: Config) -> ConfigValidationOutcome {
    if let Err(error) = validate_config(&mut draft) {
        return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
            diagnostics: vec![config_error_diagnostic(&error)],
        });
    }
    let lua_preview = match render_lua(&draft) {
        Ok(preview) => preview,
        Err(error) => {
            return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![render_error_diagnostic(&error)],
            });
        }
    };

    ConfigValidationOutcome::Valid(Box::new(ValidatedConfigDraft {
        candidate_revision: ConfigRevision::from_bytes(lua_preview.as_bytes()),
        normalized_config: draft,
        lua_preview,
        diagnostics: Vec::new(),
    }))
}

fn conflict(
    expected_revision: &ConfigRevision,
    disk_revision: ConfigRevision,
    candidate: ValidatedConfigDraft,
) -> ConfigSaveOutcome {
    let ValidatedConfigDraft {
        candidate_revision,
        normalized_config,
        lua_preview,
        ..
    } = candidate;
    ConfigSaveOutcome::Conflict(ConfigConflict {
        expected_revision: expected_revision.clone(),
        disk_revision,
        candidate_revision,
        normalized_config,
        lua_preview,
        diagnostics: vec![diagnostic(
            "E_REVISION_CONFLICT",
            ConfigDiagnosticStage::Conflict,
            "canonical configuration changed since the expected revision",
        )],
    })
}

fn failed_save(
    disk_revision: Option<ConfigRevision>,
    candidate: ValidatedConfigDraft,
    diagnostic: ConfigDiagnostic,
) -> ConfigSaveOutcome {
    let ValidatedConfigDraft {
        candidate_revision,
        normalized_config,
        lua_preview,
        ..
    } = candidate;
    ConfigSaveOutcome::Failed(ConfigSaveFailure {
        disk_revision,
        candidate_revision,
        normalized_config,
        lua_preview,
        diagnostics: vec![diagnostic],
    })
}

fn rejected_load(
    disk_revision: Option<ConfigRevision>,
    diagnostic: ConfigDiagnostic,
) -> ConfigLoadOutcome {
    ConfigLoadOutcome::Rejected(ConfigLoadRejection {
        disk_revision,
        diagnostics: vec![diagnostic],
    })
}

fn config_error_diagnostic(error: &ConfigError) -> ConfigDiagnostic {
    if matches!(error, ConfigError::Lua(_)) {
        diagnostic(
            "E_SYNTAX",
            ConfigDiagnosticStage::Parse,
            "canonical Lua could not be decoded into a configuration",
        )
    } else {
        diagnostic(
            "E_INVALID_VALUE",
            ConfigDiagnosticStage::Validation,
            "configuration values or references are invalid",
        )
    }
}

fn render_error_diagnostic(error: &ConfigError) -> ConfigDiagnostic {
    let mut diagnostic = config_error_diagnostic(error);
    diagnostic.stage = ConfigDiagnosticStage::Render;
    diagnostic.message = "normalized configuration could not be rendered as canonical Lua";
    diagnostic
}

const fn diagnostic(
    code: &'static str,
    stage: ConfigDiagnosticStage,
    message: &'static str,
) -> ConfigDiagnostic {
    ConfigDiagnostic {
        code,
        severity: ConfigDiagnosticSeverity::Error,
        stage,
        message,
    }
}

const fn cleanup_durability_warning() -> ConfigDiagnostic {
    ConfigDiagnostic {
        code: "W_CONFIG_CLEANUP_DURABILITY",
        severity: ConfigDiagnosticSeverity::Warning,
        stage: ConfigDiagnosticStage::Sync,
        message: "configuration was committed but cleanup durability could not be confirmed",
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

#[cfg(test)]
mod tests;
