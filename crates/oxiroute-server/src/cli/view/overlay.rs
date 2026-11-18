use serde_json::Value;

use super::read::{items, text};
use crate::cli::render::{Block, Document, Field};

pub(super) fn show(value: &Value) -> Document {
    let rows = items(value, "settings")
        .iter()
        .map(|setting| {
            vec![
                Field::text("target", text(setting, "target")),
                Field::text("setting", text(setting, "setting")),
                Field::text("value", text(setting, "value")),
                Field::text("status", text(setting, "status")),
            ]
        })
        .collect();
    Document::new(vec![
        Block::Record {
            name: "overlay".into(),
            fields: vec![
                Field::text("state", text(value, "state")),
                Field::revision("revision", text(value, "revision")),
                Field::text("path", text(value, "path")),
            ],
        },
        Block::Rows {
            name: "settings".into(),
            rows,
        },
    ])
}
