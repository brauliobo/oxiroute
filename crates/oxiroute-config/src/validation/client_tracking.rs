use crate::{ClientTracking, ConfigError};

pub(super) fn validate_client_tracking(
    tracking: &mut Option<ClientTracking>,
) -> Result<(), ConfigError> {
    let Some(settings) = tracking else {
        return Ok(());
    };
    check_max_clients(settings.max_clients)?;
    check_ipv6_prefix(settings.ipv6_prefix)?;
    if *settings == ClientTracking::default() {
        *tracking = None;
    }
    Ok(())
}

fn check_max_clients(max_clients: Option<u64>) -> Result<(), ConfigError> {
    let range = ClientTracking::MIN_MAX_CLIENTS..=ClientTracking::MAX_MAX_CLIENTS;
    match max_clients {
        Some(value) if !range.contains(&value) => Err(ConfigError::InvalidClientTracking {
            pointer: "/client_tracking/max_clients",
            detail: format!("must be between {} and {}", range.start(), range.end()),
        }),
        _ => Ok(()),
    }
}

fn check_ipv6_prefix(prefix: Option<u8>) -> Result<(), ConfigError> {
    match prefix {
        Some(value) if !ClientTracking::IPV6_PREFIXES.contains(&value) => {
            Err(ConfigError::InvalidClientTracking {
                pointer: "/client_tracking/ipv6_prefix",
                detail: "must be 64 or 128".to_owned(),
            })
        }
        _ => Ok(()),
    }
}
