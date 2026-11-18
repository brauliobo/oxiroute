use serde_json::Value;

use super::read::{items, joined, number, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    let mut blocks = vec![Block::Rows {
        name: "certificates".into(),
        rows: items(value, "certificates").iter().map(row).collect(),
    }];
    if let Some(watcher) = value.get("watcher").filter(|watcher| watcher.is_object()) {
        blocks.push(Block::Record {
            name: "watcher".into(),
            fields: watcher_fields(watcher),
        });
    }
    Document::new(blocks)
}

fn row(certificate: &Value) -> Record {
    let status = certificate.get("status").unwrap_or(&Value::Null);
    vec![
        Field::text("name", text(certificate, "name")),
        Field::text("source", text(certificate, "source")),
        Field::text("dns_names", joined(certificate, "dnsNames").as_deref()),
        Field::text("expires", text(status, "expiresAt")),
        Field::text("outcome", text(status, "lastOutcome")),
        Field::text("error", text(status, "lastErrorCode")),
    ]
}

fn watcher_fields(watcher: &Value) -> Record {
    let health = watcher.get("health");
    let health = health
        .and_then(Value::as_str)
        .or_else(|| health.and_then(|health| text(health, "state")));
    vec![
        Field::text("health", health),
        Field::count("backend_errors", number(watcher, "backendErrors")),
        Field::count("rescans", number(watcher, "rescans")),
        Field::count(
            "reconciliation_failures",
            number(watcher, "reconciliationFailures"),
        ),
    ]
}
