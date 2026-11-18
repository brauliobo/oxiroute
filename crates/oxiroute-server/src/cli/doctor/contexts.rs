use super::diagnosis::{Check, Diagnosis};
use crate::cli::{
    context::Context,
    contexts::{Inspection, TokenFile, inspect},
    net::read_token,
};

pub(super) fn check(context: &Context<'_>) -> Vec<Diagnosis> {
    match inspect(context.requested_context()) {
        Ok(None) => Vec::new(),
        Ok(Some(inspection)) => of(&inspection),
        Err(error) => vec![
            Diagnosis::critical(Check::Contexts, "contexts", error.to_string())
                .fixed_by("fix the contexts file, or name an existing context with --context"),
        ],
    }
}

fn of(inspection: &Inspection) -> Vec<Diagnosis> {
    let subject = inspection.path.display().to_string();
    let selected = inspection.selected.as_deref().map_or_else(
        || "none is selected".to_owned(),
        |name| format!("{name} is selected"),
    );
    let mut diagnoses = vec![Diagnosis::ok(
        Check::Contexts,
        subject,
        format!(
            "{} contexts in a regular file with mode 0600; {selected}",
            inspection.contexts
        ),
    )];
    diagnoses.extend(inspection.token_files.iter().filter_map(token_file));
    diagnoses
}

fn token_file(file: &TokenFile) -> Option<Diagnosis> {
    let error = read_token(&file.path).err()?;
    let subject = format!("context {}", file.context);
    let fix = format!(
        "chmod 600 {} and keep the management token in it, or point the context at another token",
        file.path.display()
    );
    let diagnosis = if file.selected {
        Diagnosis::critical(Check::Contexts, subject, error.to_string())
    } else {
        Diagnosis::warning(Check::Contexts, subject, error.to_string())
    };
    Some(diagnosis.fixed_by(fix))
}
