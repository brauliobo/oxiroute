use crate::{
    ConfigDraft, ConfigError,
    defaults::{MAX_DRAIN_DEADLINE_MS, MAX_RETIRED_GENERATIONS, MIN_DRAIN_DEADLINE_MS},
};

pub(super) fn validate_retirement(config: &ConfigDraft) -> Result<(), ConfigError> {
    if let Some(retirement) = &config.retirement {
        check_deadline(
            "/retirement/drain_deadline_ms",
            retirement.drain_deadline_ms,
        )?;
        if !(1..=MAX_RETIRED_GENERATIONS).contains(&retirement.max_retired_generations) {
            return Err(invalid(
                "/retirement/max_retired_generations",
                "must be between 1 and 16",
            ));
        }
    }
    check_services(
        "http_services",
        config
            .http_services
            .iter()
            .map(|s| (&s.name, s.drain_deadline_ms)),
    )?;
    check_services(
        "forward_proxy_services",
        config
            .forward_proxy_services
            .iter()
            .map(|s| (&s.name, s.drain_deadline_ms)),
    )?;
    check_services(
        "l4_services",
        config
            .l4_services
            .iter()
            .map(|s| (&s.name, s.drain_deadline_ms)),
    )
}

fn check_services<'a>(
    collection: &str,
    services: impl Iterator<Item = (&'a String, Option<u64>)>,
) -> Result<(), ConfigError> {
    for (name, deadline) in services {
        check_deadline(&format!("/{collection}/{name}/drain_deadline_ms"), deadline)?;
    }
    Ok(())
}

fn check_deadline(pointer: &str, deadline: Option<u64>) -> Result<(), ConfigError> {
    match deadline {
        Some(ms) if !(MIN_DRAIN_DEADLINE_MS..=MAX_DRAIN_DEADLINE_MS).contains(&ms) => Err(invalid(
            pointer,
            "must be between 1000 and 86400000 milliseconds",
        )),
        _ => Ok(()),
    }
}

fn invalid(pointer: &str, detail: &'static str) -> ConfigError {
    ConfigError::InvalidRetirement {
        pointer: pointer.to_owned(),
        detail,
    }
}
