use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    let rows = items(value, "servers")
        .iter()
        .map(|entry| {
            let (pool, server) = split(entry);
            row(pool, server)
        })
        .collect();
    Document::rows("servers", rows)
}

pub(super) fn detail(value: &Value) -> Document {
    let blocks = items(value, "servers")
        .iter()
        .map(|entry| {
            let (pool, server) = split(entry);
            let mut fields = row(pool, server);
            fields.extend(diagnostics(server));
            Block::Record {
                name: "server".into(),
                fields,
            }
        })
        .collect();
    Document::new(blocks)
}

pub(super) fn endpoints(pool: &Value) -> Vec<Record> {
    items(pool, "endpoints")
        .iter()
        .map(|server| row(None, server))
        .collect()
}

fn split(entry: &Value) -> (Option<&str>, &Value) {
    let server = entry.get("server").filter(|server| server.is_object());
    (text(entry, "pool"), server.unwrap_or(entry))
}

fn row(pool: Option<&str>, server: &Value) -> Record {
    let mut fields = Vec::new();
    if let Some(pool) = pool {
        fields.push(Field::text("pool", Some(pool)));
    }
    fields.extend([
        Field::text("server", text(server, "name")),
        Field::text("address", text(server, "address")),
        Field::text("admin", text(server, "administrativeState")),
        Field::text("health", text(server, "state")),
        Field::text("override", text(server, "healthOverride")),
        Field::count("weight", number(server, "weight")),
        Field::count("active", number(server, "activeConnections")),
        Field::count("max", number(server, "maxConnections")),
        Field::text("origin", text(server, "origin")),
    ]);
    fields
}

fn diagnostics(server: &Value) -> Record {
    vec![
        Field::flag("checks", flag(server, "checksEnabled")),
        Field::count("checks_succeeded", number(server, "successfulChecks")),
        Field::count("checks_failed", number(server, "failedChecks")),
        Field::count(
            "consecutive_failures",
            number(server, "consecutiveFailures"),
        ),
        Field::text("last_failure", text(server, "lastFailure")),
        Field::flag("ejected", flag(server, "passiveEjected")),
        Field::text("ejection_reason", text(server, "passiveEjectionReason")),
    ]
}
