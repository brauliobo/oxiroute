use std::{fmt::Write as _, path::Path};

use super::report;
use crate::{
    GenerationManager,
    cli::{error::CliError, fs::ensure_readable_config},
    config::coordinator::{
        CanonicalConfigCoordinator, ConfigLoadOutcome, RejectionDiagnostic, ResolvedConfigDocument,
    },
    rtmp::api::{management_token_file, preflight_management_token},
};

pub(super) fn report(path: &Path) -> Result<String, CliError> {
    ensure_readable_config(path)?;
    let coordinator = CanonicalConfigCoordinator::new(path).map_err(CliError::local)?;
    let mut document = match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => {
            let diagnostics = rejection
                .diagnostics
                .iter()
                .map(RejectionDiagnostic::from)
                .collect::<Vec<_>>();
            return Err(CliError::local(report::invalid(
                &path.display().to_string(),
                &serde_json::json!({ "diagnostics": diagnostics }),
            )));
        }
    };
    let mut notices = document
        .validated_config
        .warnings()
        .iter()
        .map(|warning| format!("warning: {warning}"))
        .collect::<Vec<_>>();
    notices.extend(without_unusable_management(&mut document)?);
    GenerationManager::new()
        .validate_candidate(*document)
        .map_err(|error| {
            CliError::local(format!(
                "configuration cannot be prepared as a complete runtime generation: {error}"
            ))
        })?;
    let mut report = String::from("configuration is valid and runtime-preparable\n");
    for notice in notices {
        writeln!(report, "{notice}").map_err(CliError::local)?;
    }
    Ok(report)
}

fn without_unusable_management(
    document: &mut ResolvedConfigDocument,
) -> Result<Option<String>, CliError> {
    let Some(management) = &document.validated_config.as_draft().management else {
        return Ok(None);
    };
    if let Some(token_file) = management_token_file(management) {
        preflight_management_token(&token_file).map_err(|error| {
            CliError::local(format!(
                "management token file {}: {error}",
                token_file.display()
            ))
        })?;
        return Ok(None);
    }
    let mut draft = document.validated_config.to_draft();
    draft.management = None;
    document.validated_config = draft.validate().map_err(CliError::local)?;
    Ok(Some(
        "warning: the management endpoint was not checked: it has no token file, set \
         management.token_file or OXIROUTE_MANAGEMENT_TOKEN_FILE before serving"
            .to_owned(),
    ))
}
