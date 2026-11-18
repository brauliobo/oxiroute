use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::{
    render::{Document, Field, Record},
    units,
};

pub(super) fn list(value: &Value) -> Document {
    let more = flag(value, "hasMore").unwrap_or(false).then(|| {
        let cursor = number(value, "cursor").unwrap_or_default();
        format!("more records are available; continue with --after {cursor}")
    });
    Document::rows("records", items(value, "records").iter().map(row).collect()).noting(more)
}

fn row(record: &Value) -> Record {
    vec![
        Field::number("id", number(record, "id")),
        Field::timestamp("time", number(record, "timestampUnixMs")),
        Field::text("actor", text(record, "actor")),
        Field::text("category", text(record, "category")),
        Field::text("operation", text(record, "operation")),
        Field::text("result", text(record, "result")),
        Field::revision("revision", text(record, "revision")),
        Field::text("correlation", text(record, "correlationId")),
    ]
}

pub(super) fn status(value: &Value) -> Document {
    let audit = value.get("audit").unwrap_or(&Value::Null);
    Document::record(
        "audit",
        vec![
            Field::text("state", text(audit, "state")),
            Field::flag("persistent", flag(audit, "persistent")),
            Field::flag("degraded", flag(audit, "degraded")),
            Field::text("last_error", text(audit, "lastError")),
            Field::count("records", number(audit, "recordCount")),
            Field::bytes("size", number(audit, "bytes")),
            Field::count("rotated_files", number(audit, "rotatedFiles")),
            Field::count("write_failures", number(audit, "writeFailures")),
            Field::count("corrupt_records", number(audit, "corruptRecords")),
            Field::count("max_records", number(audit, "maxRecords")),
            Field::bytes("max_file_size", number(audit, "maxFileBytes")),
            Field::bytes("max_total_size", number(audit, "maxTotalBytes")),
            Field::count("max_rotated_files", number(audit, "maxRotatedFiles")),
            Field::shown(
                "used",
                units::percent(
                    number(audit, "bytes").unwrap_or_default(),
                    number(audit, "maxTotalBytes").unwrap_or_default(),
                )
                .map_or_else(|| "-".into(), |used| format!("{used:.1}%")),
                None,
            ),
        ],
    )
}
