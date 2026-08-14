use std::path::PathBuf;

use oxiroute_config::ValidatedConfig;
use oxiroute_config_source::ConfigFormat;
use serde::Serialize;

use super::{ConfigDiagnostic, EffectiveRevision};

/// A validated configuration and the exact format-appropriate bytes ready for durable storage.
///
/// The representation is owned by [`super::CanonicalConfigCoordinator`]. Callers can inspect a
/// candidate, but cannot construct one or alter the prepared bytes before saving it.
///
/// ```compile_fail
/// use oxiroute_server::config_coordinator::PersistableConfigCandidate;
///
/// fn replace_preview(candidate: &mut PersistableConfigCandidate) {
///     candidate.config_preview = String::new();
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PersistableConfigCandidate {
    #[serde(rename = "candidate_revision")]
    effective_revision: EffectiveRevision,
    #[serde(rename = "normalized_config")]
    validated_config: ValidatedConfig,
    format: ConfigFormat,
    compositional: bool,
    #[serde(skip)]
    dependencies: Vec<PathBuf>,
    config_preview: String,
    diagnostics: Vec<ConfigDiagnostic>,
}

impl PersistableConfigCandidate {
    pub(super) fn new(
        effective_revision: EffectiveRevision,
        validated_config: ValidatedConfig,
        format: ConfigFormat,
        compositional: bool,
        dependencies: Vec<PathBuf>,
        config_preview: String,
        diagnostics: Vec<ConfigDiagnostic>,
    ) -> Self {
        Self {
            effective_revision,
            validated_config,
            format,
            compositional,
            dependencies,
            config_preview,
            diagnostics,
        }
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        EffectiveRevision,
        ValidatedConfig,
        ConfigFormat,
        bool,
        Vec<PathBuf>,
        String,
    ) {
        (
            self.effective_revision,
            self.validated_config,
            self.format,
            self.compositional,
            self.dependencies,
            self.config_preview,
        )
    }

    #[must_use]
    pub const fn effective_revision(&self) -> &EffectiveRevision {
        &self.effective_revision
    }

    #[must_use]
    pub const fn validated_config(&self) -> &ValidatedConfig {
        &self.validated_config
    }

    #[must_use]
    pub const fn format(&self) -> ConfigFormat {
        self.format
    }

    #[must_use]
    pub const fn compositional(&self) -> bool {
        self.compositional
    }

    #[must_use]
    pub fn dependencies(&self) -> &[PathBuf] {
        &self.dependencies
    }

    #[must_use]
    pub fn config_preview(&self) -> &str {
        &self.config_preview
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[ConfigDiagnostic] {
        &self.diagnostics
    }
}
