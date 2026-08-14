use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use openssl::sha::sha256;
use oxiroute_config::{ConfigDraft, ValidatedConfig, compose_validated_configs};
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, NativeReferenceMetadata, ResolvedSource, render_config,
    resolve_source,
};
use serde::Serialize;

mod candidate;
mod storage;

use crate::encoding::lower_hex;
pub use candidate::PersistableConfigCandidate;
use storage::{CanonicalStorage, ReplaceControl, ReplaceResult};

/// The canonical file is bounded to the same one-MiB limit as the restricted Lua loader.
pub const MAX_CANONICAL_CONFIG_BYTES: usize = 1024 * 1024;

/// A SHA-256 revision of exact authored bytes stored in the canonical file.
///
/// ```compile_fail
/// use oxiroute_server::config_coordinator::{AuthoredRevision, EffectiveRevision};
///
/// let authored = "0000000000000000000000000000000000000000000000000000000000000000"
///     .parse::<AuthoredRevision>()
///     .unwrap();
/// let _: EffectiveRevision = authored;
/// ```
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AuthoredRevision(String);

impl AuthoredRevision {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self(lower_hex(&sha256(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthoredRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AuthoredRevision")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for AuthoredRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for AuthoredRevision {
    type Err = RevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !is_lower_sha256(value) {
            return Err(RevisionParseError);
        }
        Ok(Self(value.to_owned()))
    }
}

/// A SHA-256 revision of the normalized canonical KDL representation.
///
/// ```compile_fail
/// use oxiroute_server::config_coordinator::{AuthoredRevision, EffectiveRevision};
///
/// let effective = "0000000000000000000000000000000000000000000000000000000000000000"
///     .parse::<EffectiveRevision>()
///     .unwrap();
/// let _: AuthoredRevision = effective;
/// ```
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EffectiveRevision(String);

impl EffectiveRevision {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self(lower_hex(&sha256(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EffectiveRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EffectiveRevision")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for EffectiveRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for EffectiveRevision {
    type Err = RevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !is_lower_sha256(value) {
            return Err(RevisionParseError);
        }
        Ok(Self(value.to_owned()))
    }
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("configuration revision must be a lowercase 64-character SHA-256 hexadecimal value")]
pub struct RevisionParseError;

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

/// A valid resolved source. The authored revision hashes exact disk bytes, while the effective
/// revision hashes the normalized deterministic KDL representation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedConfigDocument {
    #[serde(rename = "disk_revision")]
    pub authored_revision: AuthoredRevision,
    #[serde(rename = "candidate_revision")]
    pub effective_revision: EffectiveRevision,
    #[serde(rename = "normalized_config")]
    pub validated_config: ValidatedConfig,
    pub format: ConfigFormat,
    pub compositional: bool,
    #[serde(skip)]
    pub dependencies: Vec<PathBuf>,
    pub config_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

/// The native evidence retained by a resolved canonical source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeImportSourceDocument {
    pub disk_revision: AuthoredRevision,
    pub candidate_revision: EffectiveRevision,
    pub format: ConfigFormat,
    pub compositional: bool,
    pub native_references: Vec<NativeReferenceMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeImportSourceOutcome {
    Loaded(Box<NativeImportSourceDocument>),
    Rejected(ConfigLoadRejection),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLoadOutcome {
    Loaded(Box<ResolvedConfigDocument>),
    Rejected(ConfigLoadRejection),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigLoadRejection {
    pub disk_revision: Option<AuthoredRevision>,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValidationOutcome {
    Valid(Box<PersistableConfigCandidate>),
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
    Saved(ResolvedConfigDocument),
    Conflict(ConfigConflict),
    InvalidDraft(ConfigDraftRejection),
    Failed(ConfigSaveFailure),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigConflict {
    pub expected_revision: AuthoredRevision,
    pub disk_revision: AuthoredRevision,
    pub candidate_revision: EffectiveRevision,
    pub normalized_config: ValidatedConfig,
    pub format: ConfigFormat,
    pub compositional: bool,
    #[serde(skip)]
    pub dependencies: Vec<PathBuf>,
    pub config_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigSaveFailure {
    pub disk_revision: Option<AuthoredRevision>,
    pub candidate_revision: EffectiveRevision,
    pub normalized_config: ValidatedConfig,
    pub format: ConfigFormat,
    pub compositional: bool,
    #[serde(skip)]
    pub dependencies: Vec<PathBuf>,
    pub config_preview: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigCoordinatorPathError {
    #[error("canonical configuration path must identify one file")]
    MissingFileName,
    #[error("canonical configuration path has an unsupported source format")]
    UnsupportedFormat,
}

/// Coordinates reads, validation, previews, and revision-checked durable saves for one canonical
/// source path. Runtime activation and its revision remain the caller's responsibility.
#[derive(Clone)]
pub struct CanonicalConfigCoordinator {
    canonical_path: PathBuf,
    format: ConfigFormat,
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
        let format = ConfigFormat::infer(&canonical_path)
            .map_err(|_| ConfigCoordinatorPathError::UnsupportedFormat)?;
        Ok(Self {
            canonical_path,
            format,
            operation_lock: Arc::new(Mutex::new(())),
        })
    }

    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    #[must_use]
    pub const fn format(&self) -> ConfigFormat {
        self.format
    }

    /// Reads one stable no-follow snapshot and returns its normalized typed representation.
    #[must_use]
    pub fn load(&self) -> ConfigLoadOutcome {
        let (disk_revision, resolved) = match self.read_resolved_source() {
            Ok(resolved) => resolved,
            Err(rejection) => return ConfigLoadOutcome::Rejected(rejection),
        };
        let config_preview = match render_config(resolved.format, &resolved.config) {
            Ok(preview) => preview,
            Err(error) => {
                return ConfigLoadOutcome::Rejected(ConfigLoadRejection {
                    disk_revision: Some(disk_revision),
                    diagnostics: vec![source_error_diagnostic(&error)],
                });
            }
        };

        ConfigLoadOutcome::Loaded(Box::new(ResolvedConfigDocument {
            authored_revision: disk_revision,
            effective_revision: EffectiveRevision::from_bytes(resolved.canonical_kdl.as_bytes()),
            validated_config: resolved.config,
            format: resolved.format,
            compositional: resolved.compositional,
            dependencies: resolved.dependencies,
            config_preview,
            diagnostics: Vec::new(),
        }))
    }

    /// Reads the same stable source snapshot as [`Self::load`] while retaining native import evidence.
    #[must_use]
    pub fn load_native_import_source(&self) -> NativeImportSourceOutcome {
        let (disk_revision, resolved) = match self.read_resolved_source() {
            Ok(resolved) => resolved,
            Err(rejection) => return NativeImportSourceOutcome::Rejected(rejection),
        };
        NativeImportSourceOutcome::Loaded(Box::new(NativeImportSourceDocument {
            disk_revision,
            candidate_revision: EffectiveRevision::from_bytes(resolved.canonical_kdl.as_bytes()),
            format: resolved.format,
            compositional: resolved.compositional,
            native_references: resolved.native_references,
        }))
    }

    fn read_resolved_source(
        &self,
    ) -> Result<(AuthoredRevision, ResolvedSource), ConfigLoadRejection> {
        let _operation = self
            .operation_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let storage =
            CanonicalStorage::open(&self.canonical_path).map_err(|error| ConfigLoadRejection {
                disk_revision: None,
                diagnostics: vec![error.diagnostic()],
            })?;
        let disk = storage.read().map_err(|error| ConfigLoadRejection {
            disk_revision: None,
            diagnostics: vec![error.diagnostic()],
        })?;
        let disk_revision = AuthoredRevision::from_bytes(&disk);
        let resolved =
            resolve_source(&self.canonical_path, &disk).map_err(|error| ConfigLoadRejection {
                disk_revision: Some(disk_revision.clone()),
                diagnostics: vec![source_error_diagnostic(&error)],
            })?;
        Ok((disk_revision, resolved))
    }

    /// Validates and normalizes a typed draft without reading or writing the canonical file.
    #[must_use]
    pub fn prepare(&self, draft: ConfigDraft) -> ConfigValidationOutcome {
        prepare_candidate(self.format, draft)
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
    pub fn save(
        &self,
        expected_revision: &AuthoredRevision,
        candidate: PersistableConfigCandidate,
    ) -> ConfigSaveOutcome {
        self.save_inner(
            expected_revision,
            candidate,
            || Ok(()),
            || {},
            ReplaceControl::default(),
        )
    }

    fn save_inner<F, G>(
        &self,
        expected_revision: &AuthoredRevision,
        candidate: PersistableConfigCandidate,
        before_exchange: F,
        after_exchange: G,
        control: ReplaceControl,
    ) -> ConfigSaveOutcome
    where
        F: FnOnce() -> Result<(), ()>,
        G: FnOnce(),
    {
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
        let initial_revision = AuthoredRevision::from_bytes(&initial_disk);
        if initial_revision != *expected_revision {
            return conflict(expected_revision, initial_revision, candidate);
        }
        match resolve_source(&self.canonical_path, &initial_disk) {
            Ok(resolved) if resolved.compositional => {
                return ConfigSaveOutcome::InvalidDraft(ConfigDraftRejection {
                    diagnostics: vec![compositional_root_diagnostic()],
                });
            }
            Ok(_) => {}
            Err(error) => {
                return failed_save(
                    Some(initial_revision),
                    candidate,
                    source_error_diagnostic(&error),
                );
            }
        }

        match storage.replace(
            &transaction,
            expected_revision,
            candidate.config_preview().as_bytes(),
            before_exchange,
            after_exchange,
            control,
        ) {
            Ok(ReplaceResult::Saved { cleanup_degraded }) => {
                let (
                    effective_revision,
                    validated_config,
                    format,
                    compositional,
                    dependencies,
                    config_preview,
                ) = candidate.into_parts();
                ConfigSaveOutcome::Saved(ResolvedConfigDocument {
                    authored_revision: AuthoredRevision::from_bytes(config_preview.as_bytes()),
                    effective_revision,
                    validated_config,
                    format,
                    compositional,
                    dependencies,
                    config_preview,
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
                    .map(|disk| AuthoredRevision::from_bytes(&disk));
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

fn prepare_candidate(format: ConfigFormat, draft: ConfigDraft) -> ConfigValidationOutcome {
    let Ok(validated_config) = compose_validated_configs(vec![draft]) else {
        return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
            diagnostics: vec![diagnostic(
                "E_INVALID_VALUE",
                ConfigDiagnosticStage::Validation,
                "configuration values or references are invalid",
            )],
        });
    };
    let canonical_kdl = match render_config(ConfigFormat::Kdl, &validated_config) {
        Ok(preview) => preview,
        Err(error) => {
            return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&error)],
            });
        }
    };
    let config_preview = match render_config(format, &validated_config) {
        Ok(preview) => preview,
        Err(error) => {
            return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&error)],
            });
        }
    };

    ConfigValidationOutcome::Valid(Box::new(PersistableConfigCandidate::new(
        EffectiveRevision::from_bytes(canonical_kdl.as_bytes()),
        validated_config,
        format,
        false,
        Vec::new(),
        config_preview,
        Vec::new(),
    )))
}

fn source_error_diagnostic(error: &ConfigSourceError) -> ConfigDiagnostic {
    match error {
        ConfigSourceError::SourceTooLarge => diagnostic(
            "E_CONFIG_TOO_LARGE",
            ConfigDiagnosticStage::Read,
            "canonical configuration exceeds the one-MiB limit",
        ),
        ConfigSourceError::Utf8(_)
        | ConfigSourceError::Parse { .. }
        | ConfigSourceError::Lua(_) => diagnostic(
            "E_SYNTAX",
            ConfigDiagnosticStage::Parse,
            "canonical configuration could not be decoded",
        ),
        ConfigSourceError::Render { .. } | ConfigSourceError::OutputTooLarge => diagnostic(
            "E_RENDER",
            ConfigDiagnosticStage::Render,
            "normalized configuration could not be rendered in the canonical format",
        ),
        ConfigSourceError::NativeImport { .. } => diagnostic(
            "E_NATIVE_SOURCE",
            ConfigDiagnosticStage::Validation,
            "a referenced native configuration could not be resolved",
        ),
        _ => diagnostic(
            "E_INVALID_VALUE",
            ConfigDiagnosticStage::Validation,
            "configuration values, composition, or references are invalid",
        ),
    }
}

const fn compositional_root_diagnostic() -> ConfigDiagnostic {
    diagnostic(
        "E_COMPOSITIONAL_ROOT",
        ConfigDiagnosticStage::Validation,
        "typed saves cannot replace a compositional configuration root",
    )
}

fn conflict(
    expected_revision: &AuthoredRevision,
    disk_revision: AuthoredRevision,
    candidate: PersistableConfigCandidate,
) -> ConfigSaveOutcome {
    let (effective_revision, validated_config, format, compositional, dependencies, config_preview) =
        candidate.into_parts();
    ConfigSaveOutcome::Conflict(ConfigConflict {
        expected_revision: expected_revision.clone(),
        disk_revision,
        candidate_revision: effective_revision,
        normalized_config: validated_config,
        format,
        compositional,
        dependencies,
        config_preview,
        diagnostics: vec![diagnostic(
            "E_REVISION_CONFLICT",
            ConfigDiagnosticStage::Conflict,
            "canonical configuration changed since the expected revision",
        )],
    })
}

fn failed_save(
    disk_revision: Option<AuthoredRevision>,
    candidate: PersistableConfigCandidate,
    diagnostic: ConfigDiagnostic,
) -> ConfigSaveOutcome {
    let (effective_revision, validated_config, format, compositional, dependencies, config_preview) =
        candidate.into_parts();
    ConfigSaveOutcome::Failed(ConfigSaveFailure {
        disk_revision,
        candidate_revision: effective_revision,
        normalized_config: validated_config,
        format,
        compositional,
        dependencies,
        config_preview,
        diagnostics: vec![diagnostic],
    })
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

#[cfg(test)]
mod tests;
