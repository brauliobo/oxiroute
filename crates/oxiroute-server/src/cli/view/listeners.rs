use serde_json::Value;

use super::read::{items, number, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    Document::rows(
        "listeners",
        items(value, "listeners").iter().map(row).collect(),
    )
}

pub(super) fn detail(value: &Value) -> Document {
    let blocks = items(value, "listeners")
        .iter()
        .map(|listener| Block::Record {
            name: "listener".into(),
            fields: row(listener),
        })
        .collect();
    Document::new(blocks)
}

pub(super) fn row(listener: &Value) -> Record {
    let bind = text(listener, "bind").map(|bind| bind.strip_prefix("socket:").unwrap_or(bind));
    vec![
        Field::text("name", text(listener, "name")),
        Field::text("protocol", text(listener, "protocol")),
        Field::text("bind", bind),
        Field::text("admin", text(listener, "administrativeState")),
        Field::text("state", text(listener, "state")),
        Field::count("active", number(listener, "activeConnections")),
        Field::count("max", number(listener, "maxConnections")),
        Field::count("accepted", number(listener, "acceptedConnections")),
        Field::count("rejected", number(listener, "rejectedConnections")),
        Field::bytes("received", number(listener, "bytesReceived")),
        Field::bytes("sent", number(listener, "bytesSent")),
    ]
}
