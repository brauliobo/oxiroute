use std::fmt::Write as _;

use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Document, Field, Record};

pub(super) fn status(value: &Value) -> Document {
    let generation = value.get("generation").unwrap_or(value);
    Document::record("generation", fields(generation))
}

pub(super) fn fields(generation: &Value) -> Record {
    vec![
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
    ]
}

fn rejection(generation: &Value) -> Option<String> {
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
