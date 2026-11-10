use std::ops::RangeInclusive;

use crate::{ConfigDraft, ConfigError};

pub(super) fn validate_worker_scaling(config: &ConfigDraft) -> Result<(), ConfigError> {
    let tunables = [
        ("min_active_workers", config.min_active_workers),
        ("target_busy_percent", config.target_busy_percent),
        ("scale_down_delay_ms", config.scale_down_delay_ms),
    ];
    if !config.dynamic_worker_scaling() {
        return match tunables.into_iter().find(|(_, value)| value.is_some()) {
            Some((name, _)) => Err(ConfigError::WorkerScalingTunableWithoutDynamic(name)),
            None => Ok(()),
        };
    }
    if !config.aged_per_worker() {
        return Err(ConfigError::DynamicWorkerScalingWithoutAgedScheduling);
    }
    check_range(
        config.min_active_workers,
        1..=ConfigDraft::MAX_MIN_ACTIVE_WORKERS,
    )
    .map_err(ConfigError::InvalidMinActiveWorkers)?;
    check_range(
        config.target_busy_percent,
        ConfigDraft::MIN_TARGET_BUSY_PERCENT..=ConfigDraft::MAX_TARGET_BUSY_PERCENT,
    )
    .map_err(ConfigError::InvalidTargetBusyPercent)?;
    check_range(
        config.scale_down_delay_ms,
        ConfigDraft::MIN_SCALE_DOWN_DELAY_MS..=ConfigDraft::MAX_SCALE_DOWN_DELAY_MS,
    )
    .map_err(ConfigError::InvalidScaleDownDelay)?;
    let workers = config.listener_workers.unwrap_or(1);
    let min_active = config.effective_min_active_workers();
    if min_active > workers {
        return Err(ConfigError::MinActiveWorkersAboveListenerWorkers {
            min_active,
            workers,
        });
    }
    Ok(())
}

fn check_range(value: Option<u64>, range: RangeInclusive<u64>) -> Result<(), u64> {
    match value {
        Some(value) if !range.contains(&value) => Err(value),
        _ => Ok(()),
    }
}
