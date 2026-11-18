use serde_json::Value;

use super::{
    read::{items, number, text},
    servers,
};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    Document::rows("pools", items(value, "pools").iter().map(row).collect())
}

pub(super) fn detail(value: &Value) -> Document {
    let blocks = items(value, "pools")
        .iter()
        .flat_map(|pool| {
            [
                Block::Record {
                    name: "pool".into(),
                    fields: row(pool),
                },
                Block::Rows {
                    name: "servers".into(),
                    rows: servers::endpoints(pool),
                },
            ]
        })
        .collect();
    Document::new(blocks)
}

pub(super) fn row(pool: &Value) -> Record {
    vec![
        Field::text("name", text(pool, "name")),
        Field::text("algorithm", text(pool, "algorithm")),
        Field::text("admin", administrative_state(pool).as_deref()),
        Field::count("available", number(pool, "availableEndpoints")),
        Field::count("servers", number(pool, "totalEndpoints")),
        Field::count("queued", number(pool, "queued")),
        Field::count("queue_timeouts", number(pool, "queueTimeouts")),
    ]
}

pub(super) fn administrative_state(pool: &Value) -> Option<String> {
    let mut states = items(pool, "endpoints")
        .iter()
        .filter_map(|endpoint| text(endpoint, "administrativeState"));
    let first = states.next()?;
    Some(
        if states.all(|state| state == first) {
            first
        } else {
            "mixed"
        }
        .to_owned(),
    )
}
