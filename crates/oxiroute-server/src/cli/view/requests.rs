use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    let shown = items(value, "requests").len();
    let truncated = flag(value, "truncated").unwrap_or(false);
    let notice = truncated.then(|| {
        let total = number(value, "total").unwrap_or_default();
        format!("showing {shown} of {total} requests; raise --limit to see more")
    });
    Document::rows(
        "requests",
        items(value, "requests").iter().map(row).collect(),
    )
    .noting(notice)
}

pub(super) fn detail(value: &Value) -> Document {
    let blocks = items(value, "requests")
        .iter()
        .map(|request| {
            let mut fields = row(request);
            fields.extend([
                Field::text("listener", text(request, "listener")),
                Field::text("route", text(request, "route")),
                Field::text("client", text(request, "clientIp")),
                Field::millis("waited", number(request, "waitedMs")),
                Field::number("queue_position", number(request, "queuePosition")),
                Field::flag("overdue", flag(request, "overdue")),
                Field::timestamp("started", number(request, "startedAtUnixMs")),
            ]);
            Block::Record {
                name: "request".into(),
                fields,
            }
        })
        .collect();
    Document::new(blocks)
}

fn row(request: &Value) -> Record {
    vec![
        Field::text("id", text(request, "requestId")),
        Field::duration("age", number(request, "ageMs")),
        Field::text("phase", text(request, "phase")),
        Field::text("pool", text(request, "pool")),
        Field::text("server", text(request, "server")),
        Field::text("method", text(request, "method")),
        Field::text("host", text(request, "host")),
        Field::text("path", text(request, "path")),
        Field::number("attempts", number(request, "attempts")),
    ]
}
