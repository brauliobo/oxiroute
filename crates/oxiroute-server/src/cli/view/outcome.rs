use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn evacuation(value: &Value) -> Document {
    let left_running = value
        .get("leftRunning")
        .and_then(Value::as_object)
        .map(|counts| {
            counts
                .values()
                .filter_map(|count| count.as_u64().or_else(|| count.as_str()?.parse().ok()))
                .sum()
        });
    Document::record(
        "evacuation",
        vec![
            Field::text("outcome", text(value, "outcome")),
            Field::text("persistence", text(value, "persistence")),
            Field::flag("force", flag(value, "force")),
            Field::number("redispatched", number(value, "cancelledAndRedispatched")),
            Field::number("forced", number(value, "forced")),
            Field::number("left_running", left_running),
        ],
    )
}

pub(super) fn dns_refresh(value: &Value) -> Document {
    let summary = vec![
        Field::text("outcome", text(value, "outcome")),
        Field::text("persistence", text(value, "persistence")),
        Field::flag("atomic", flag(value, "atomic")),
    ];
    Document::new(vec![
        Block::Record {
            name: "refresh".into(),
            fields: summary,
        },
        Block::Rows {
            name: "servers".into(),
            rows: items(value, "servers").iter().map(server).collect(),
        },
    ])
}

fn server(server: &Value) -> Record {
    let error = server.get("error").and_then(|error| text(error, "code"));
    vec![
        Field::text("pool", text(server, "pool")),
        Field::text("server", text(server, "server")),
        Field::text("outcome", text(server, "outcome")),
        Field::text("error", error),
    ]
}
