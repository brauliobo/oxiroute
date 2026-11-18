use serde_json::Value;

use super::{
    listeners, pools,
    read::{items, number, text},
};
use crate::cli::render::{Block, Document, Field};

pub(super) fn show(value: &Value) -> Document {
    let null = Value::Null;
    let process = value.get("process").unwrap_or(&null);
    let host = value.get("host").unwrap_or(&null);
    let traffic = value.get("traffic").unwrap_or(&null);
    let cpu = process.get("cpuPercent").and_then(Value::as_f64);
    let load = host.get("loadAverage1m").and_then(Value::as_f64);
    let fields = vec![
        Field::duration("uptime", number(value, "uptimeMs")),
        Field::duration("generation_age", number(value, "generationAgeMs")),
        Field::text("admin", text(process, "administrativeState")),
        Field::percent("cpu", cpu),
        Field::bytes("memory", number(process, "residentMemoryBytes")),
        Field::count("threads", number(process, "threadCount")),
        Field::count("fds", number(process, "openFileDescriptors")),
        Field::count("connections", number(traffic, "activeConnections")),
        Field::count("accepted", number(traffic, "acceptedConnections")),
        Field::count("rejected", number(traffic, "rejectedConnections")),
        Field::count("retries", number(process, "retryAttempts")),
        Field::bytes("received", number(traffic, "bytesReceived")),
        Field::bytes("sent", number(traffic, "bytesSent")),
        Field::shown(
            "host_load",
            load.map_or_else(|| "-".to_owned(), |load| format!("{load:.2}")),
            load.map(|load| load.to_string()),
        ),
        Field::bytes(
            "host_memory_available",
            number(host, "availableMemoryBytes"),
        ),
        Field::bytes("host_memory_total", number(host, "totalMemoryBytes")),
    ];
    Document::new(vec![
        Block::Record {
            name: "process".into(),
            fields,
        },
        Block::Rows {
            name: "listeners".into(),
            rows: items(value, "listeners")
                .iter()
                .map(listeners::row)
                .collect(),
        },
        Block::Rows {
            name: "pools".into(),
            rows: items(value, "upstreamPools")
                .iter()
                .map(pools::row)
                .collect(),
        },
    ])
}
