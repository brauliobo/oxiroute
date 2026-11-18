use serde_json::Value;

use super::read::{items, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn show(value: &Value) -> Document {
    let state = value.get("state").unwrap_or(&Value::Null);
    let summary = vec![
        Field::text("config", text(state, "config")),
        Field::text("runtime", text(state, "runtime")),
    ];
    Document::new(vec![
        Block::Record {
            name: "topology".into(),
            fields: summary,
        },
        Block::Rows {
            name: "nodes".into(),
            rows: items(value, "nodes").iter().map(node).collect(),
        },
        Block::Rows {
            name: "edges".into(),
            rows: items(value, "edges").iter().map(edge).collect(),
        },
    ])
}

fn node(node: &Value) -> Record {
    vec![
        Field::text("kind", text(node, "kind")),
        Field::text("name", text(node, "name")),
        Field::text("config_path", text(node, "configPath")),
    ]
}

fn edge(edge: &Value) -> Record {
    vec![
        Field::text("kind", text(edge, "kind")),
        Field::text("source", text(edge, "source")),
        Field::text("target", text(edge, "target")),
    ]
}
