use crate::{ConfigDraft, ConfigError, Stats, ValidatedConfig};

#[derive(Debug, thiserror::Error)]
pub enum ConfigCompositionError {
    #[error("at least one canonical configuration is required")]
    Empty,
    #[error("canonical configurations disagree on process-wide field `{field}`")]
    ProcessFieldConflict { field: &'static str },
    #[error(transparent)]
    Invalid(#[from] ConfigError),
}

/// Composes complete configuration drafts in input order and returns validated state.
///
/// Process-wide settings must agree when more than one input specifies them. Draft fragments are
/// merged before canonical validation, allowing references to be completed by another draft.
///
/// # Errors
///
/// Returns an error when no input is supplied, process-wide fields conflict, or the resulting
/// canonical configuration is invalid.
pub fn compose_validated_configs(
    configs: Vec<ConfigDraft>,
) -> Result<ValidatedConfig, ConfigCompositionError> {
    compose_drafts(configs.into_iter())?
        .validate()
        .map_err(ConfigCompositionError::from)
}

fn compose_drafts(
    mut configs: impl Iterator<Item = ConfigDraft>,
) -> Result<ConfigDraft, ConfigCompositionError> {
    let Some(mut composed) = configs.next() else {
        return Err(ConfigCompositionError::Empty);
    };

    for config in configs {
        merge_process_fields(&mut composed, &config)?;
        composed.certificates.extend(config.certificates);
        composed.tls_profiles.extend(config.tls_profiles);
        composed.listeners.extend(config.listeners);
        composed.cache_stores.extend(config.cache_stores);
        composed.upstream_pools.extend(config.upstream_pools);
        composed.http_services.extend(config.http_services);
        composed
            .forward_proxy_services
            .extend(config.forward_proxy_services);
        composed.rtmp_services.extend(config.rtmp_services);
        composed.l4_services.extend(config.l4_services);
    }

    Ok(composed)
}

fn merge_process_fields(
    composed: &mut ConfigDraft,
    config: &ConfigDraft,
) -> Result<(), ConfigCompositionError> {
    if composed.version != config.version {
        return Err(ConfigCompositionError::ProcessFieldConflict { field: "version" });
    }
    merge_runtime_tuning_fields(composed, config)?;
    merge_optional(
        "management",
        &mut composed.management,
        config.management.as_ref(),
    )?;
    merge_optional("audit", &mut composed.audit, config.audit.as_ref())?;
    merge_optional(
        "config_history",
        &mut composed.config_history,
        config.config_history.as_ref(),
    )?;
    merge_optional(
        "client_tracking",
        &mut composed.client_tracking,
        config.client_tracking.as_ref(),
    )?;
    merge_optional(
        "log_filter",
        &mut composed.log_filter,
        config.log_filter.as_ref(),
    )?;
    merge_optional(
        "cpu_level",
        &mut composed.cpu_level,
        config.cpu_level.as_ref(),
    )?;
    merge_optional("registry", &mut composed.registry, config.registry.as_ref())?;
    merge_stats(&mut composed.stats, config.stats.as_ref())?;
    Ok(())
}

fn merge_runtime_tuning_fields(
    composed: &mut ConfigDraft,
    config: &ConfigDraft,
) -> Result<(), ConfigCompositionError> {
    merge_optional_copy(
        "max_connections",
        &mut composed.max_connections,
        config.max_connections,
    )?;
    merge_optional_copy(
        "listener_workers",
        &mut composed.listener_workers,
        config.listener_workers,
    )?;
    merge_optional_copy(
        "http_fast_lane",
        &mut composed.http_fast_lane,
        config.http_fast_lane,
    )?;
    merge_optional_copy(
        "http_data_plane",
        &mut composed.http_data_plane,
        config.http_data_plane,
    )?;
    merge_optional_copy(
        "connection_scheduling",
        &mut composed.connection_scheduling,
        config.connection_scheduling,
    )?;
    merge_optional_copy(
        "connection_age_ms",
        &mut composed.connection_age_ms,
        config.connection_age_ms,
    )?;
    merge_optional_copy(
        "worker_scaling",
        &mut composed.worker_scaling,
        config.worker_scaling,
    )?;
    merge_optional_copy(
        "min_active_workers",
        &mut composed.min_active_workers,
        config.min_active_workers,
    )?;
    merge_optional_copy(
        "target_busy_percent",
        &mut composed.target_busy_percent,
        config.target_busy_percent,
    )?;
    merge_optional_copy(
        "scale_down_delay_ms",
        &mut composed.scale_down_delay_ms,
        config.scale_down_delay_ms,
    )?;
    merge_optional_copy(
        "tls_handshake_threads",
        &mut composed.tls_handshake_threads,
        config.tls_handshake_threads,
    )?;
    merge_optional_copy(
        "http3_congestion_control",
        &mut composed.http3_congestion_control,
        config.http3_congestion_control,
    )?;
    merge_optional_copy(
        "http3_segmentation_offload",
        &mut composed.http3_segmentation_offload,
        config.http3_segmentation_offload,
    )?;
    merge_optional(
        "retirement",
        &mut composed.retirement,
        config.retirement.as_ref(),
    )?;
    Ok(())
}

fn merge_optional_copy<T: Copy + Eq>(
    field: &'static str,
    target: &mut Option<T>,
    incoming: Option<T>,
) -> Result<(), ConfigCompositionError> {
    match (*target, incoming) {
        (None, Some(value)) => *target = Some(value),
        (Some(current), Some(value)) if current != value => {
            return Err(ConfigCompositionError::ProcessFieldConflict { field });
        }
        _ => {}
    }
    Ok(())
}

fn merge_optional<T: Clone + Eq>(
    field: &'static str,
    target: &mut Option<T>,
    incoming: Option<&T>,
) -> Result<(), ConfigCompositionError> {
    match (&*target, incoming) {
        (None, Some(value)) => *target = Some(value.clone()),
        (Some(current), Some(value)) if current != value => {
            return Err(ConfigCompositionError::ProcessFieldConflict { field });
        }
        _ => {}
    }
    Ok(())
}

fn merge_stats(
    target: &mut Option<Stats>,
    incoming: Option<&Stats>,
) -> Result<(), ConfigCompositionError> {
    let Some(incoming) = incoming else {
        return Ok(());
    };
    let Some(target) = target else {
        *target = Some(incoming.clone());
        return Ok(());
    };
    merge_optional(
        "stats.admin_token_file",
        &mut target.admin_token_file,
        incoming.admin_token_file.as_ref(),
    )?;
    target.binds.extend(incoming.binds.iter().copied());
    target.pages.extend(incoming.pages.clone());
    Ok(())
}

#[cfg(test)]
mod tests;
