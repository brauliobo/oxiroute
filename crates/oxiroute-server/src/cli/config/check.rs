use std::{fmt::Write as _, path::Path};

use oxiroute_config_source::{ConfigFormat, render_config};

use super::report;
use crate::{
    GenerationManager,
    cli::{error::CliError, fs::ensure_readable_config},
    config::coordinator::{
        CanonicalConfigCoordinator, ConfigLoadOutcome, RejectionDiagnostic, ResolvedConfigDocument,
    },
    rtmp::api::{management_token_file, preflight_management_token},
};

pub(in crate::cli) fn load(path: &Path) -> Result<Box<ResolvedConfigDocument>, CliError> {
    ensure_readable_config(path)?;
    let coordinator = CanonicalConfigCoordinator::new(path).map_err(CliError::local)?;
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => Ok(document),
        ConfigLoadOutcome::Rejected(rejection) => {
            let diagnostics = rejection
                .diagnostics
                .iter()
                .map(RejectionDiagnostic::from)
                .collect::<Vec<_>>();
            Err(CliError::local(report::invalid(
                &path.display().to_string(),
                &serde_json::json!({ "diagnostics": diagnostics }),
            )))
        }
    }
}

pub(in crate::cli) fn warnings(document: &ResolvedConfigDocument) -> Vec<String> {
    document
        .validated_config
        .warnings()
        .iter()
        .map(ToString::to_string)
        .collect()
}

pub(in crate::cli) fn prepare(
    document: ResolvedConfigDocument,
    probe_listeners: bool,
) -> Result<(), CliError> {
    let manager = GenerationManager::new();
    if probe_listeners {
        manager.validate_candidate(document)
    } else {
        manager.validate_candidate_without_listener_probes(document)
    }
    .map_err(|error| {
        CliError::local(format!(
            "configuration cannot be prepared as a complete runtime generation: {error}"
        ))
    })
}

pub(in crate::cli) fn without_management(
    document: &mut ResolvedConfigDocument,
) -> Result<(), CliError> {
    let mut draft = document.validated_config.to_draft();
    draft.management = None;
    document.validated_config = draft.validate().map_err(CliError::local)?;
    Ok(())
}

pub(in crate::cli) fn report(
    path: &Path,
    probe_listeners: bool,
    dump_format: Option<ConfigFormat>,
) -> Result<String, CliError> {
    let mut document = load(path)?;
    let resolved = dump_format.map(|_| document.validated_config.clone());
    let mut notices = warnings(&document);
    notices.extend(without_unusable_management(&mut document)?);
    prepare(*document, probe_listeners)?;
    let mut report = String::from("configuration is valid and runtime-preparable\n");
    for notice in notices {
        writeln!(report, "warning: {notice}").map_err(CliError::local)?;
    }
    if let Some((format, validated)) = dump_format.zip(resolved) {
        let rendered = render_config(format, &validated).map_err(CliError::local)?;
        report.push('\n');
        report.push_str(&rendered);
        if !report.ends_with('\n') {
            report.push('\n');
        }
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
    without_management(document)?;
    Ok(Some(
        "the management endpoint was not checked: it has no token file, set \
         management.token_file or OXIROUTE_MANAGEMENT_TOKEN_FILE before serving"
            .to_owned(),
    ))
}
