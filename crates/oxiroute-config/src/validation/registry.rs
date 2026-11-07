#[allow(clippy::wildcard_imports)]
use super::*;

const MAX_REGISTRY_ENDPOINTS: usize = 8;

pub(super) fn validate_registry(config: &ConfigDraft) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRegistry { field, detail };
    let Some(registry) = &config.registry else {
        if config
            .upstream_pools
            .iter()
            .any(|pool| pool.registration.is_some())
        {
            return Err(invalid(
                "state_dir",
                "a pool with `registration` needs a `registry` block",
            ));
        }
        return Ok(());
    };
    if !registry.state_dir.is_absolute() {
        return Err(invalid("state_dir", "must be an absolute path"));
    }
    if registry
        .tokens_file
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(invalid("tokens_file", "must be an absolute path"));
    }
    if registry.endpoints.len() > MAX_REGISTRY_ENDPOINTS {
        return Err(invalid("endpoints", "at most 8 endpoints"));
    }
    for endpoint in &registry.endpoints {
        match endpoint {
            RegistryEndpoint::Unix { path, mode, .. } => {
                if !path.is_absolute() {
                    return Err(invalid("endpoints", "unix socket path must be absolute"));
                }
                if *mode > 0o777 || mode & 0o002 != 0 {
                    return Err(invalid(
                        "endpoints",
                        "unix socket mode must not be world-writable",
                    ));
                }
            }
            RegistryEndpoint::Tcp { bind, allow_cidrs } => {
                if bind.port() == 0 {
                    return Err(invalid("endpoints", "tcp bind needs a non-zero port"));
                }
                if registry.tokens_file.is_none() {
                    return Err(invalid(
                        "tokens_file",
                        "tcp endpoints require a tokens_file",
                    ));
                }
                if allow_cidrs
                    .iter()
                    .any(|cidr| cidr_contains(cidr, IpAddr::from([0, 0, 0, 0])).is_none())
                {
                    return Err(invalid(
                        "endpoints",
                        "allow_cidrs entries must be `address/prefix`",
                    ));
                }
            }
        }
    }
    Ok(())
}
