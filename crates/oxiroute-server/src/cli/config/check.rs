use std::{fmt::Write as _, path::Path};

use oxiroute_config::ConfigWarning;

use crate::{
    GenerationManager,
    cli::error::CliError,
    config::coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};

pub(super) fn report(path: &Path) -> Result<String, CliError> {
    let mut report = String::from("configuration is valid and runtime-preparable\n");
    for warning in check(path)? {
        writeln!(report, "warning: {warning}").map_err(CliError::startup)?;
    }
    Ok(report)
}

fn check(path: &Path) -> Result<Vec<ConfigWarning>, CliError> {
    let coordinator = CanonicalConfigCoordinator::new(path).map_err(CliError::startup)?;
    let document = match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => {
            return Err(CliError::startup(rejection.message()));
        }
    };
    let warnings = document.validated_config.warnings();
    GenerationManager::new()
        .validate_candidate(*document)
        .map_err(|error| {
            CliError::startup(format!(
                "configuration cannot be prepared as a complete runtime generation: {error}"
            ))
        })?;
    Ok(warnings)
}
