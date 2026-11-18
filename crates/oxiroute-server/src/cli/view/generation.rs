use std::fmt::Write as _;

use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Document, Field, Record};

pub(super) fn status(value: &Value) -> Document {
    let generation = value.get("generation").unwrap_or(value);
    Document::record("generation", fields(generation))
}

pub(super) fn fields(generation: &Value) -> Record {
    let mut fields = vec![
        Field::text("build", text(generation, "buildVersion")),
        Field::count("generation", number(generation, "generation")),
        Field::revision("active", text(generation, "activeRevision")),
        Field::revision("previous", text(generation, "previousRevision")),
        Field::revision("candidate", text(generation, "candidateRevision")),
        Field::revision("disk", text(generation, "diskRevision")),
        Field::revision("quarantined", text(generation, "quarantinedRevision")),
        Field::flag("accepting", flag(generation, "activeAccepting")),
        Field::flag("degraded", flag(generation, "degraded")),
        Field::text("last_failure", text(generation, "lastFailure")),
        Field::text("last_rejection", rejection(generation).as_deref()),
        Field::text("pending_apply", pending(generation).as_deref()),
        Field::count("prepares", number(generation, "prepares")),
        Field::count("activations", number(generation, "activations")),
        Field::count("rejections", number(generation, "configRejections")),
        Field::count("failures", number(generation, "failures")),
        Field::count("rollbacks", number(generation, "rollbacks")),
        Field::count(
            "retired",
            u64::try_from(items(generation, "retiredGenerations").len()).ok(),
        ),
    ];
    with_restart_banner(&mut fields, generation);
    fields
}

/// Puts the banner of a daemon that holds settings back until a restart first: the first reason and
/// how many more.
pub(super) fn with_restart_banner(fields: &mut Record, generation: &Value) {
    let Some(restart) = generation
        .get("restartRequired")
        .filter(|restart| restart.get("required").and_then(Value::as_bool) == Some(true))
    else {
        return;
    };
    let reasons = items(restart, "reasons");
    let Some(first) = reasons.first() else {
        return;
    };
    let mut banner = format!(
        "RESTART REQUIRED: {}: {}",
        text(first, "code").unwrap_or("restart"),
        text(first, "summary").unwrap_or_default(),
    );
    if reasons.len() > 1 {
        let _ = write!(banner, " (+{} more)", reasons.len() - 1);
    }
    fields.insert(0, Field::text("restart_required", Some(&banner)));
}

pub(super) fn rejection(generation: &Value) -> Option<String> {
    let rejection = generation
        .get("lastRejection")
        .filter(|value| value.is_object())?;
    let diagnostics = items(rejection, "diagnostics");
    let first = diagnostics.first()?;
    let code = text(first, "code").unwrap_or("rejected");
    let message = text(first, "message").filter(|message| !message.is_empty());
    let mut summary =
        message.map_or_else(|| code.to_owned(), |message| format!("{code}: {message}"));
    if diagnostics.len() > 1 {
        let _ = write!(summary, " (+{} more)", diagnostics.len() - 1);
    }
    Some(summary)
}

fn pending(generation: &Value) -> Option<String> {
    let pending = generation
        .get("pendingApply")
        .filter(|value| value.is_object())?;
    text(pending, "reason").map(str::to_owned)
}
