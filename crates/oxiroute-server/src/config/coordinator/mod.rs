use std::{
    fmt::{self, Write as _},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use openssl::sha::sha256;
use oxiroute_config::{
    ConfigCompositionError, ConfigDraft, ValidatedConfig, compose_validated_configs,
};
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, LuaConfigError, NativeReferenceMetadata, ResolvedSource,
    render_config, resolve_source,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod cache;
mod candidate;
mod storage;

use crate::encoding::lower_hex;
use cache::LoadCache;
pub use candidate::PersistableConfigCandidate;
use storage::{CanonicalStorage, ReplaceControl, ReplaceResult};

/// The canonical file is bounded to the same four-MiB limit as every config source and rendering. A router-sized
/// import is about 1.5 MiB because each static route carries its own MIME table.
pub const MAX_CANONICAL_CONFIG_BYTES: usize = 4 * 1024 * 1024;

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

impl ConfigDiagnosticStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Parse => "parse",
            Self::Validation => "validation",
            Self::Render => "render",
            Self::Conflict => "conflict",
            Self::Write => "write",
            Self::Sync => "sync",
            Self::Rollback => "rollback",
        }
    }
}

/// A deliberately redacted diagnostic suitable for returning across a management boundary.
///
/// Diagnostics never retain source text, configured paths, OS error strings, or rejected values.
/// They may name the failing setting and the rule it broke.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigDiagnostic {
    pub code: &'static str,
    pub severity: ConfigDiagnosticSeverity,
    pub stage: ConfigDiagnosticStage,
    pub message: &'static str,
    /// JSON pointer to the failing setting, with item names in place of list indexes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The validator's explanation of the failing setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// One-based line of a syntax error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
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

impl ConfigLoadRejection {
    /// The one-line reason the file was refused, for startup failures and offline checks.
    #[must_use]
    pub fn message(&self) -> String {
        ConfigRejection::new(self.disk_revision.as_ref(), &self.diagnostics).message()
    }
}

/// The newest configuration the process refused to run and why.
///
/// Reported by `generation status` until a later configuration is accepted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigRejection {
    pub at_unix_ms: u64,
    /// SHA-256 of the refused file, when it could be read.
    pub revision: Option<String>,
    pub diagnostics: Vec<RejectionDiagnostic>,
}

/// An owned [`ConfigDiagnostic`], so that a rejection can cross the master/worker boundary.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RejectionDiagnostic {
    pub code: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl From<&ConfigDiagnostic> for RejectionDiagnostic {
    fn from(diagnostic: &ConfigDiagnostic) -> Self {
        Self {
            code: diagnostic.code.to_owned(),
            stage: diagnostic.stage.as_str().to_owned(),
            message: diagnostic.message.to_owned(),
            path: diagnostic.path.clone(),
            detail: diagnostic.detail.clone(),
            line: diagnostic.line,
        }
    }
}

impl RejectionDiagnostic {
    /// A stable code, the failing setting or line, and the explanation, on one line.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = self.code.clone();
        if let Some(reason) = self
            .detail
            .as_deref()
            .or(Some(&self.message))
            .filter(|reason| !reason.is_empty())
        {
            let _ = write!(summary, ": {reason}");
        }
        if let Some(path) = &self.path {
            let _ = write!(summary, " at {path}");
        }
        if let Some(line) = self.line {
            let _ = write!(summary, " (line {line})");
        }
        summary
    }
}

impl ConfigRejection {
    /// The one-line reason a file was refused, for startup failures and offline checks.
    #[must_use]
    pub fn message(&self) -> String {
        let summaries = self
            .diagnostics
            .iter()
            .map(RejectionDiagnostic::summary)
            .collect::<Vec<_>>()
            .join("; ");
        if summaries.is_empty() {
            "canonical configuration was rejected".to_owned()
        } else {
            format!("canonical configuration was rejected ({summaries})")
        }
    }

    /// Logs the refusal once per diagnostic, naming the setting and never quoting the source.
    pub fn log(&self) {
        let revision = self.revision.as_deref().unwrap_or("unreadable");
        for diagnostic in &self.diagnostics {
            log::error!(
                target: "oxiroute::config",
                "configuration file revision {revision} rejected, the running configuration is \
                 unchanged: {}",
                diagnostic.summary()
            );
        }
    }

    #[must_use]
    pub fn new(revision: Option<&AuthoredRevision>, diagnostics: &[ConfigDiagnostic]) -> Self {
        Self {
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
                }),
            revision: revision.map(ToString::to_string),
            diagnostics: diagnostics.iter().map(Into::into).collect(),
        }
    }
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
    operation_lock: Arc<Mutex<LoadCache>>,
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
            operation_lock: Arc::new(Mutex::new(LoadCache::default())),
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
        match self.load_shared() {
            Ok(document) => ConfigLoadOutcome::Loaded(Box::new(Arc::unwrap_or_clone(document))),
            Err(rejection) => ConfigLoadOutcome::Rejected(rejection),
        }
    }

    /// Like [`Self::load`], without copying the document.
    ///
    /// The file is always read and hashed; the parse, validation and rendering are reused while the
    /// bytes are unchanged and the source reads no other file.
    ///
    /// # Errors
    ///
    /// Returns the redacted rejection when the snapshot cannot be read or resolved.
    pub fn load_shared(&self) -> Result<Arc<ResolvedConfigDocument>, ConfigLoadRejection> {
        let mut cache = self.lock();
        let (disk, disk_revision) = self.read_disk()?;
        if let Some(document) = cache.get(&disk_revision) {
            return Ok(document);
        }
        let resolved = self.resolve_disk(&disk, &disk_revision)?;
        let config_preview = if resolved.format == ConfigFormat::Kdl {
            resolved.canonical_kdl.clone()
        } else {
            render_config(resolved.format, &resolved.config).map_err(|error| {
                rejection(Some(disk_revision.clone()), source_error_diagnostic(&error))
            })?
        };
        Ok(cache.store(ResolvedConfigDocument {
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
        let _operation = self.lock();
        let resolved = self
            .read_disk()
            .and_then(|(disk, revision)| Ok((self.resolve_disk(&disk, &revision)?, revision)));
        match resolved {
            Ok((resolved, disk_revision)) => {
                NativeImportSourceOutcome::Loaded(Box::new(NativeImportSourceDocument {
                    disk_revision,
                    candidate_revision: EffectiveRevision::from_bytes(
                        resolved.canonical_kdl.as_bytes(),
                    ),
                    format: resolved.format,
                    compositional: resolved.compositional,
                    native_references: resolved.native_references,
                }))
            }
            Err(rejection) => NativeImportSourceOutcome::Rejected(rejection),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LoadCache> {
        self.operation_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read_disk(&self) -> Result<(Vec<u8>, AuthoredRevision), ConfigLoadRejection> {
        let storage = CanonicalStorage::open(&self.canonical_path)
            .map_err(|error| rejection(None, error.diagnostic()))?;
        let disk = storage
            .read()
            .map_err(|error| rejection(None, error.diagnostic()))?;
        let revision = AuthoredRevision::from_bytes(&disk);
        Ok((disk, revision))
    }

    fn resolve_disk(
        &self,
        disk: &[u8],
        revision: &AuthoredRevision,
    ) -> Result<ResolvedSource, ConfigLoadRejection> {
        resolve_source(&self.canonical_path, disk)
            .map_err(|error| rejection(Some(revision.clone()), source_error_diagnostic(&error)))
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
        let _operation = self.lock();
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

fn rejection(
    disk_revision: Option<AuthoredRevision>,
    diagnostic: ConfigDiagnostic,
) -> ConfigLoadRejection {
    ConfigLoadRejection {
        disk_revision,
        diagnostics: vec![diagnostic],
    }
}

fn prepare_candidate(format: ConfigFormat, draft: ConfigDraft) -> ConfigValidationOutcome {
    let validated_config = match compose_validated_configs(vec![draft]) {
        Ok(validated_config) => validated_config,
        Err(error) => {
            return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&ConfigSourceError::Composition(
                    error,
                ))],
            });
        }
    };
    let canonical_kdl = match render_config(ConfigFormat::Kdl, &validated_config) {
        Ok(preview) => preview,
        Err(error) => {
            return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&error)],
            });
        }
    };
    let config_preview = if format == ConfigFormat::Kdl {
        canonical_kdl.clone()
    } else {
        match render_config(format, &validated_config) {
            Ok(preview) => preview,
            Err(error) => {
                return ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                    diagnostics: vec![source_error_diagnostic(&error)],
                });
            }
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
    let (path, detail, line) = source_error_location(error);
    ConfigDiagnostic {
        path,
        detail,
        line,
        ..source_error_class(error)
    }
}

/// Locates the failing setting without quoting source text.
///
/// Only parsers whose messages are fixed phrases (KDL) and validators that name settings
/// contribute a detail; HOCON, UCI and Lua syntax errors contribute at most a line.
fn source_error_location(
    error: &ConfigSourceError,
) -> (Option<String>, Option<String>, Option<u32>) {
    match error {
        ConfigSourceError::Parse {
            format: "KDL 2",
            message,
            line,
        } => (None, Some(message.clone()), *line),
        ConfigSourceError::Parse { line, .. } => (None, None, *line),
        ConfigSourceError::Composition(ConfigCompositionError::Invalid(error))
        | ConfigSourceError::Lua(LuaConfigError::Config(error)) => {
            (error.pointer(), Some(error.to_string()), None)
        }
        ConfigSourceError::Lua(LuaConfigError::Lua(error)) => (None, None, lua_line(error)),
        ConfigSourceError::TypedConfig(message) => (None, Some(without_values(message)), None),
        ConfigSourceError::Composition(_)
        | ConfigSourceError::Template(_)
        | ConfigSourceError::NativeImport { .. }
        | ConfigSourceError::StructuralDepth
        | ConfigSourceError::ExpansionDepth
        | ConfigSourceError::SubstitutionLimit
        | ConfigSourceError::NodeLimit
        | ConfigSourceError::StringLimit => (None, Some(error.to_string()), None),
        _ => (None, None, None),
    }
}

/// Serde quotes the rejected value in type and value errors; keep only what was expected.
pub(crate) fn without_values(message: &str) -> String {
    if message.starts_with("invalid type: ") || message.starts_with("invalid value: ") {
        let kind = message.split_once(':').map_or("value", |(kind, _)| kind);
        let expected = message.split_once(", expected ").map_or("", |(_, e)| e);
        return format!("{kind}, expected {expected}");
    }
    message.to_owned()
}

fn lua_line(error: &impl fmt::Display) -> Option<u32> {
    let message = error.to_string();
    let (_, rest) = message.split_once("oxiroute.lua:")?;
    rest.split(':').next()?.parse().ok()
}

fn source_error_class(error: &ConfigSourceError) -> ConfigDiagnostic {
    match error {
        ConfigSourceError::SourceTooLarge => diagnostic(
            "E_CONFIG_TOO_LARGE",
            ConfigDiagnosticStage::Read,
            "canonical configuration exceeds the four-MiB limit",
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
        path: None,
        detail: None,
        line: None,
    }
}

const fn cleanup_durability_warning() -> ConfigDiagnostic {
    ConfigDiagnostic {
        code: "W_CONFIG_CLEANUP_DURABILITY",
        severity: ConfigDiagnosticSeverity::Warning,
        stage: ConfigDiagnosticStage::Sync,
        message: "configuration was committed but cleanup durability could not be confirmed",
        path: None,
        detail: None,
        line: None,
    }
}

#[cfg(test)]
mod tests;
