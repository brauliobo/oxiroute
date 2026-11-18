use serde::Deserialize;

use super::{Cell, Column, Row, Tone};
use crate::{cli::units, rtmp::api::dto::DecimalCounter};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli::top) struct Requests {
    pub(in crate::cli::top) total: DecimalCounter,
    pub(in crate::cli::top) truncated: bool,
    #[serde(rename = "requests")]
    pub(in crate::cli::top) items: Vec<Request>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli::top) struct Request {
    #[serde(rename = "requestId")]
    id: String,
    listener: String,
    pool: Option<String>,
    server: Option<String>,
    method: String,
    host: Option<String>,
    path: String,
    client_ip: Option<String>,
    phase: String,
    age_ms: DecimalCounter,
    waited_ms: Option<DecimalCounter>,
    attempts: u32,
    overdue: bool,
}

pub(super) fn columns() -> Vec<Column> {
    vec![
        Column::text("ID"),
        Column::text("LISTENER"),
        Column::text("CLIENT"),
        Column::text("METHOD"),
        Column::text("HOST"),
        Column::text("PATH"),
        Column::text("POOL"),
        Column::text("SERVER"),
        Column::text("PHASE"),
        Column::number("AGE"),
        Column::number("WAIT"),
        Column::number("TRY"),
    ]
}

pub(in crate::cli::top) fn rows(requests: &Requests) -> Vec<Row> {
    requests.items.iter().map(row).collect()
}

fn optional(value: Option<&String>) -> Cell {
    value.map_or_else(Cell::missing, Cell::text)
}

fn row(request: &Request) -> Row {
    let age = request.age_ms.get();
    let wait = request.waited_ms.map_or_else(Cell::missing, |wait| {
        Cell::number(units::millis(wait.get()), units::as_f64(wait.get()))
    });
    let age_tone = if request.overdue {
        Tone::Bad
    } else {
        Tone::Plain
    };
    let cells = vec![
        Cell::text(&request.id),
        Cell::text(&request.listener),
        optional(request.client_ip.as_ref()),
        Cell::text(&request.method),
        optional(request.host.as_ref()),
        Cell::text(&request.path),
        optional(request.pool.as_ref()),
        optional(request.server.as_ref()),
        Cell::text(&request.phase),
        Cell::number(units::millis(age), units::as_f64(age)).toned(age_tone),
        wait,
        Cell::number(request.attempts.to_string(), f64::from(request.attempts)),
    ];
    let names = [
        request.id.as_str(),
        request.listener.as_str(),
        request.client_ip.as_deref().unwrap_or_default(),
        request.host.as_deref().unwrap_or_default(),
        request.path.as_str(),
        request.pool.as_deref().unwrap_or_default(),
        request.server.as_deref().unwrap_or_default(),
    ];
    Row::new(cells, &names)
}
