use crate::{ConfigError, ConfigHistory};

pub(super) fn validate_config_history(
    history: &mut Option<ConfigHistory>,
) -> Result<(), ConfigError> {
    let Some(settings) = history else {
        return Ok(());
    };
    check(
        "/config_history/max_entries",
        settings.max_entries,
        1..=ConfigHistory::MAX_ENTRIES,
    )?;
    check(
        "/config_history/max_total_bytes",
        settings.max_total_bytes,
        ConfigHistory::MIN_TOTAL_BYTES..=ConfigHistory::MAX_TOTAL_BYTES,
    )?;
    if *settings == ConfigHistory::default() {
        *history = None;
    }
    Ok(())
}

fn check(
    pointer: &'static str,
    value: Option<u64>,
    range: std::ops::RangeInclusive<u64>,
) -> Result<(), ConfigError> {
    match value {
        Some(value) if !range.contains(&value) => Err(ConfigError::InvalidConfigHistory {
            pointer,
            detail: format!("must be between {} and {}", range.start(), range.end()),
        }),
        _ => Ok(()),
    }
}
