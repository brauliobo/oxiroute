use serde::Serialize;

use crate::{ConfigDraft, ConfigError, validation::validate_config_in_place};

/// An immutable configuration that completed canonical validation and normalization.
///
/// The inner draft is intentionally inaccessible for mutation or construction outside this crate.
///
/// ```compile_fail
/// use oxiroute_config::{ConfigDraft, ValidatedConfig};
///
/// let draft: ConfigDraft = todo!();
/// let _ = ValidatedConfig(draft);
/// ```
///
/// ```compile_fail
/// use oxiroute_config::ValidatedConfig;
///
/// fn require_deserialize<T: serde::de::DeserializeOwned>() {}
/// require_deserialize::<ValidatedConfig>();
/// ```
///
/// ```compile_fail
/// use oxiroute_config::{ConfigDraft, ValidatedConfig};
///
/// fn inner_mut(config: &mut ValidatedConfig) -> &mut ConfigDraft {
///     config
/// }
/// ```
///
/// ```compile_fail
/// use oxiroute_config::ValidatedConfig;
///
/// fn clear_listeners(config: &ValidatedConfig) {
///     config.as_draft().listeners.clear();
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ValidatedConfig(ConfigDraft);

impl ConfigDraft {
    /// Consumes, normalizes, and validates this complete configuration draft.
    ///
    /// No draft is returned on failure, so partially normalized state cannot escape.
    ///
    /// # Errors
    ///
    /// Returns an error when any configured value or cross-reference is invalid.
    ///
    /// ```compile_fail
    /// use oxiroute_config::ConfigDraft;
    ///
    /// fn validate_and_reuse(draft: ConfigDraft) {
    ///     let _validated = draft.validate().unwrap();
    ///     let _reused = draft.clone();
    /// }
    /// ```
    pub fn validate(mut self) -> Result<ValidatedConfig, ConfigError> {
        validate_config_in_place(&mut self)?;
        Ok(ValidatedConfig(self))
    }
}

impl ValidatedConfig {
    /// Borrows the normalized draft without granting mutation.
    #[must_use]
    pub const fn as_draft(&self) -> &ConfigDraft {
        &self.0
    }

    /// Clones the normalized value into a mutable draft for an explicit new transition.
    #[must_use]
    pub fn to_draft(&self) -> ConfigDraft {
        self.0.clone()
    }
}
