use std::path::Path;

use oxiroute_config::ConfigDraft;

use super::diagnosis::{Check, Diagnosis};
use crate::cli::config::check::{load, prepare, warnings, without_management};

pub(super) fn check(path: &Path) -> Result<(ConfigDraft, Vec<Diagnosis>), Diagnosis> {
    let subject = path.display().to_string();
    let failed = |error: crate::cli::error::CliError| {
        Diagnosis::critical(Check::Config, &subject, error.to_string()).fixed_by(format!(
            "edit it, then run `oxiroute config check {subject}`"
        ))
    };
    let mut document = load(path).map_err(failed)?;
    let draft = document.validated_config.to_draft();
    let mut diagnoses = vec![Diagnosis::ok(
        Check::Config,
        &subject,
        "the configuration is valid and runtime-preparable",
    )];
    diagnoses.extend(
        warnings(&document)
            .into_iter()
            .map(|warning| Diagnosis::warning(Check::Config, &subject, warning)),
    );
    without_management(&mut document).map_err(failed)?;
    prepare(*document, false).map_err(failed)?;
    Ok((draft, diagnoses))
}
