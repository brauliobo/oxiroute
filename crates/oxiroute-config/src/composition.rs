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
        if composed.version != config.version {
            return Err(ConfigCompositionError::ProcessFieldConflict { field: "version" });
        }
        merge_optional_copy(
            "max_connections",
            &mut composed.max_connections,
            config.max_connections,
        )?;
        merge_optional(
            "management",
            &mut composed.management,
            config.management.as_ref(),
        )?;
        merge_optional("audit", &mut composed.audit, config.audit.as_ref())?;
        merge_optional("registry", &mut composed.registry, config.registry.as_ref())?;
        merge_stats(&mut composed.stats, config.stats.as_ref())?;
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
