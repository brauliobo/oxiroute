use serde_json::Value;

use super::read::{items, number, text};
use crate::cli::render::{Block, Document, Field, Record};

pub(super) fn list(value: &Value) -> Document {
    let as_of = number(value, "as_of_unix_ms");
    Document::rows(
        "streams",
        items(value, "streams")
            .iter()
            .map(|stream| row(stream, as_of))
            .collect(),
    )
}

pub(super) fn detail(value: &Value) -> Document {
    let as_of = number(value, "as_of_unix_ms");
    let streams = value.get("streams").and_then(Value::as_array);
    let blocks = streams
        .map_or_else(|| vec![value], |streams| streams.iter().collect())
        .into_iter()
        .map(|stream| {
            let mut fields = row(stream, as_of);
            fields.extend([
                Field::text("server", text(stream, "server_id")),
                Field::revision("revision", text(stream, "revision")),
                Field::count("relays", u64::try_from(items(stream, "relays").len()).ok()),
                Field::count(
                    "recorders",
                    u64::try_from(items(stream, "recorders").len()).ok(),
                ),
            ]);
            Block::Record {
                name: "stream".into(),
                fields,
            }
        })
        .collect();
    Document::new(blocks)
}

pub(super) fn clients(value: &Value) -> Document {
    let as_of = number(value, "asOfUnixMs");
    Document::rows(
        "sessions",
        items(value, "clients")
            .iter()
            .map(|client| {
                let age = number(client, "connectedAtUnixMs")
                    .zip(as_of)
                    .and_then(|(connected, now)| now.checked_sub(connected));
                vec![
                    Field::text("id", text(client, "id")),
                    Field::text("service", text(client, "service")),
                    Field::text("application", text(client, "application")),
                    Field::text("stream", text(client, "stream")),
                    Field::text("role", text(client, "role")),
                    Field::text("peer", text(client, "peerIp")),
                    Field::duration("age", age),
                    Field::text("revision", text(client, "revision")),
                ]
            })
            .collect(),
    )
}

fn row(stream: &Value, as_of: Option<u64>) -> Record {
    let media = stream.get("media").unwrap_or(&Value::Null);
    let track = |name| {
        media
            .get(name)
            .and_then(|track| number(track, "payload_bytes"))
    };
    let publisher = stream
        .get("publisher")
        .and_then(|publisher| text(publisher, "session_id"));
    let age = number(stream, "created_at_unix_ms")
        .zip(as_of)
        .and_then(|(created, now)| now.checked_sub(created));
    vec![
        Field::text("id", text(stream, "id")),
        Field::text("application", text(stream, "application")),
        Field::text("name", text(stream, "name")),
        Field::text("publisher", publisher),
        Field::number("subscribers", number(stream, "subscriber_count")),
        Field::bytes("audio", track("audio")),
        Field::bytes("video", track("video")),
        Field::duration("age", age),
    ]
}
