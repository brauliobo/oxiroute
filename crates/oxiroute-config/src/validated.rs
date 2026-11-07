use serde::Serialize;

use crate::{
    ConfigDraft, ConfigError,
    defaults::{DEFAULT_DRAIN_DEADLINE_MS, MAX_DRAIN_DEADLINE_MS, default_max_retired_generations},
    validation::validate_config_in_place,
};

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

impl ConfigDraft {
    /// Milliseconds a replaced generation may keep draining its in-flight work.
    ///
    /// An unset `retirement.drain_deadline_ms` follows the longest HTTP request deadline so no
    /// request is cut before its own timeout; per service deadlines only raise the result.
    #[must_use]
    pub fn retirement_deadline_ms(&self) -> u64 {
        self.retirement_deadline_ms_with_default(None)
    }

    /// Like [`Self::retirement_deadline_ms`] with `fallback` standing in for the derived default,
    /// for a deployment-wide setting that an explicit `retirement.drain_deadline_ms` overrides.
    #[must_use]
    pub fn retirement_deadline_ms_with_default(&self, fallback: Option<u64>) -> u64 {
        let derived = self
            .http_services
            .iter()
            .map(|service| service.upstream_io_timeout_ms)
            .fold(DEFAULT_DRAIN_DEADLINE_MS, u64::max);
        let general = self
            .retirement
            .as_ref()
            .and_then(|retirement| retirement.drain_deadline_ms)
            .or(fallback)
            .unwrap_or(derived)
            .min(MAX_DRAIN_DEADLINE_MS);
        let services = self
            .http_services
            .iter()
            .map(|service| service.drain_deadline_ms);
        let forward = self
            .forward_proxy_services
            .iter()
            .map(|service| service.drain_deadline_ms);
        let l4 = self
            .l4_services
            .iter()
            .map(|service| service.drain_deadline_ms);
        services
            .chain(forward)
            .chain(l4)
            .flatten()
            .fold(general, u64::max)
    }

    /// Replaced generations that may drain at once.
    ///
    /// # Panics
    ///
    /// Panics when the cap does not fit `usize`; validation bounds it to 16.
    #[must_use]
    pub fn retired_generation_limit(&self) -> usize {
        self.retirement
            .as_ref()
            .map_or_else(default_max_retired_generations, |retirement| {
                retirement.max_retired_generations
            })
            .try_into()
            .expect("validated retirement cap fits usize")
    }
}

impl ValidatedConfig {
    /// See [`ConfigDraft::retirement_deadline_ms`].
    #[must_use]
    pub fn retirement_deadline_ms(&self) -> u64 {
        self.0.retirement_deadline_ms()
    }

    /// See [`ConfigDraft::retirement_deadline_ms_with_default`].
    #[must_use]
    pub fn retirement_deadline_ms_with_default(&self, fallback: Option<u64>) -> u64 {
        self.0.retirement_deadline_ms_with_default(fallback)
    }

    /// See [`ConfigDraft::retired_generation_limit`].
    #[must_use]
    pub fn retired_generation_limit(&self) -> usize {
        self.0.retired_generation_limit()
    }

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
