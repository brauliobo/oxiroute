use oxiroute_cpu::Level;

use crate::ConfigError;

pub(super) fn validate_cpu_level(level: Option<&String>) -> Result<(), ConfigError> {
    match level {
        Some(name) if Level::parse(name).is_none() => {
            Err(ConfigError::InvalidCpuLevel(name.clone()))
        }
        _ => Ok(()),
    }
}
