use serde_json::Value;

use super::read::{items, number, text};
use crate::cli::{
    render::{Document, Field, Record},
    units,
};

pub(super) fn list(value: &Value) -> Document {
    let more = text(value, "next_cursor")
        .map(|cursor| format!("more recordings are available; continue with --cursor {cursor}"));
    Document::rows(
        "recordings",
        items(value, "recordings").iter().map(row).collect(),
    )
    .noting(more)
}

fn row(recording: &Value) -> Record {
    vec![
        Field::text("id", text(recording, "id")),
        Field::text("name", text(recording, "name")),
        Field::text("stream", text(recording, "stream")),
        Field::text("status", text(recording, "status")),
        Field::bytes("size", number(recording, "size_bytes")),
        Field::duration("duration", number(recording, "duration_ms")),
        Field::timestamp("started", number(recording, "started_at_unix_ms")),
        Field::text("recorders", recorders(recording).as_deref()),
    ]
}

fn recorders(value: &Value) -> Option<String> {
    let names = items(value, "recorders")
        .iter()
        .filter_map(|source| text(source, "recorder"))
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(","))
}

pub(super) fn detail(value: &Value) -> Document {
    let mut fields = row(value);
    fields.extend([
        Field::timestamp("ended", number(value, "ended_at_unix_ms")),
        Field::timestamp("modified", number(value, "modified_at_unix_ms")),
        Field::text("video_codec", text(value, "video_codec")),
        Field::text("audio_codec", text(value, "audio_codec")),
    ]);
    Document::record("recording", fields)
}

pub(super) fn quota(value: &Value) -> Document {
    Document::rows(
        "recording roots",
        items(value, "roots").iter().map(root).collect(),
    )
}

fn root(root: &Value) -> Record {
    let used = number(root, "bytes_used");
    let max = number(root, "max_bytes");
    vec![
        Field::text("recorders", recorders(root).as_deref()),
        Field::bytes("used", used),
        Field::bytes("max", max),
        Field::percent(
            "share",
            used.zip(max)
                .and_then(|(used, max)| units::percent(used, max)),
        ),
        Field::number("files", number(root, "files")),
        Field::number("max_files", number(root, "max_files")),
        Field::text("retention", text(root, "retention")),
        Field::duration("max_age", number(root, "retention_max_age_ms")),
        Field::number("evicted_files", number(root, "evicted_files")),
        Field::bytes("evicted", number(root, "evicted_bytes")),
    ]
}

pub(super) fn download(value: &Value) -> Document {
    Document::record(
        "download",
        vec![
            Field::text("id", text(value, "id")),
            Field::text("file", text(value, "file")),
            Field::bytes("size", number(value, "bytes")),
        ],
    )
}
