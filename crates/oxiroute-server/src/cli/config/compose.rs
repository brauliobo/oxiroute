use std::path::PathBuf;

use oxiroute_config_source::{ConfigFormat, render_config};

use crate::{
    cli::{error::CliError, fs::ensure_readable_config},
    config::coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};

pub(super) fn compose(paths: &[PathBuf], format: ConfigFormat) -> Result<String, CliError> {
    let mut configs = Vec::with_capacity(paths.len());
    for path in paths {
        ensure_readable_config(path)?;
        let coordinator = CanonicalConfigCoordinator::new(path).map_err(CliError::startup)?;
        let document = match coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document,
            ConfigLoadOutcome::Rejected(rejection) => {
                return Err(CliError::startup(format!(
                    "`{}`: {}",
                    path.display(),
                    rejection.message()
                )));
            }
        };
        configs.push(document.validated_config.to_draft());
    }
    let composed =
        oxiroute_config::compose_validated_configs(configs).map_err(CliError::startup)?;
    render_config(format, &composed).map_err(CliError::startup)
}
