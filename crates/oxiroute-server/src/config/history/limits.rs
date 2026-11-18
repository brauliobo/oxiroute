use oxiroute_config::{ConfigHistory, ValidatedConfig};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryLimits {
    pub(crate) max_entries: usize,
    pub(crate) max_total_bytes: u64,
}

impl HistoryLimits {
    pub(crate) fn of(config: &ValidatedConfig) -> Self {
        Self::from_settings(&config.as_draft().config_history.clone().unwrap_or_default())
    }

    fn from_settings(settings: &ConfigHistory) -> Self {
        Self {
            max_entries: usize::try_from(
                settings
                    .max_entries
                    .unwrap_or(ConfigHistory::DEFAULT_MAX_ENTRIES),
            )
            .unwrap_or(usize::MAX),
            max_total_bytes: settings
                .max_total_bytes
                .unwrap_or(ConfigHistory::DEFAULT_MAX_TOTAL_BYTES),
        }
    }
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self::from_settings(&ConfigHistory::default())
    }
}
