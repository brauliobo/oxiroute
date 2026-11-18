use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    let more = flag(value, "hasMore").unwrap_or(false).then(|| {
        let cursor = number(value, "cursor").unwrap_or_default();
        format!("more events are available; continue with --after {cursor}")
    });
    Document::rows("events", items(value, "events").iter().map(row).collect()).noting(more)
}

fn row(event: &Value) -> Record {
    vec![
        Field::number("cursor", number(event, "cursor")),
        Field::timestamp("time", number(event, "timestampUnixMs")),
        Field::text("event", text(event, "event")),
        Field::text("outcome", text(event, "outcome")),
        Field::text("actor", text(event, "actor")),
        Field::revision("revision", text(event, "revision")),
        Field::text("correlation", text(event, "correlationId")),
    ]
}
