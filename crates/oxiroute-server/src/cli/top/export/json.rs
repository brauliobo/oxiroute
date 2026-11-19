use serde_json::{Value, json};

use super::{Plan, Printed};
use crate::cli::{
    error::CliError,
    top::{
        history::Sampling,
        model::{Cell, Notice, Row, Unit},
        totals::Totals,
    },
};

pub(crate) const SCHEMA_VERSION: u32 = 2;

fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Count | Unit::Plain => "count",
        Unit::Rate => "perSecond",
        Unit::Bytes => "bytes",
        Unit::BytesRate => "bytesPerSecond",
        Unit::Percent => "percent",
        Unit::Millis | Unit::Duration => "milliseconds",
    }
}

fn column_unit(printed: &Printed, index: usize) -> Value {
    let found = printed
        .arranged
        .table
        .rows
        .iter()
        .find_map(|row| row.cells[index].unit());
    match (found, printed.view.columns()[index].numeric) {
        (Some(unit), _) => json!(unit_name(unit)),
        (None, true) => Value::Null,
        (None, false) => json!("text"),
    }
}

fn row(printed: &Printed, row: &Row, parent: Option<&Row>) -> Value {
    let columns = printed.view.columns();
    let values = columns
        .iter()
        .zip(&row.cells)
        .map(|(column, cell): (_, &Cell)| (column.key.to_owned(), cell.json()))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "key": row.key,
        "depth": row.depth,
        "parent": parent.map(|parent| parent.key.clone()),
        "values": values,
    })
}

fn pinned(printed: &Printed, row_value: Option<&Row>) -> Value {
    row_value.map_or(Value::Null, |found| row(printed, found, None))
}

fn rates(sampling: &Sampling) -> Value {
    let now = Totals::of(sampling.stats());
    let before = sampling.before_stats().map(Totals::of);
    let rate = |now: u64, before: Option<u64>| sampling.rate(now, before);
    json!({
        "reqPerSec": rate(now.requests, before.map(|t| t.requests)),
        "errPerSec": rate(now.errors, before.map(|t| t.errors)),
        "bytesInPerSec": rate(now.bytes_in, before.map(|t| t.bytes_in)),
        "bytesOutPerSec": rate(now.bytes_out, before.map(|t| t.bytes_out)),
        "newConnPerSec": rate(now.accepted, before.map(|t| t.accepted)),
    })
}

fn header(sampling: &Sampling, plan: &Plan) -> Value {
    let stats = sampling.stats();
    let process = &stats.process;
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "sampledAtUnixMs": stats.sampled_at_unix_ms,
        "window": {
            "name": plan.window.label(),
            "ms": plan.window.millis(),
            "complete": sampling.complete,
        },
        "instance": {
            "version": process.version,
            "uptimeMs": process.uptime_ms,
            "generation": stats.generation.number,
            "accepting": stats.generation.accepting,
            "draining": stats.generation.retired.len(),
            "warnings": stats.warnings.len(),
        },
        "summary": {
            "connections": { "current": process.active_connections, "limit": process.max_connections },
            "rates": rates(sampling),
            "process": {
                "cpuPercent": process.cpu_percent,
                "rssBytes": process.resident_memory_bytes,
                "fds": process.open_file_descriptors,
                "threads": process.thread_count,
            },
        },
    })
}

pub(super) fn render(
    sampling: &Sampling,
    plan: &Plan,
    tables: &[Printed],
) -> Result<String, CliError> {
    let Some(printed) = tables.first() else {
        return Err(CliError::Remote("the server returned no statistics".into()));
    };
    let columns = printed.view.columns();
    let table = &printed.arranged.table;
    let rows = (0..printed.arranged.len())
        .filter_map(|position| printed.arranged.row(position))
        .map(|found| {
            row(
                printed,
                found,
                found.parent.map(|parent| &table.rows[parent]),
            )
        })
        .collect::<Vec<_>>();
    let listing = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            json!({
                "key": column.key,
                "title": column.title,
                "unit": column_unit(printed, index),
                "visible": printed.columns.contains(&index),
            })
        })
        .collect::<Vec<_>>();
    let truncated = table
        .population
        .is_some_and(|population| population.of > population.shown);
    let mut document = header(sampling, plan);
    let body = json!({
        "view": printed.view.name(),
        "sort": { "column": columns[printed.sort.column].key, "descending": printed.sort.descending },
        "filter": plan.filter.source(),
        "columns": listing,
        "rows": rows,
        "other": pinned(printed, table.other.as_ref()),
        "total": pinned(printed, table.total.as_ref()),
        "truncated": truncated,
        "population": table.population.map(|population| json!({ "shown": population.shown, "of": population.of })),
        "limit": plan.top,
        "notice": table.notice.as_ref().map(Notice::text),
    });
    if let (Some(target), Some(extra)) = (document.as_object_mut(), body.as_object()) {
        target.extend(extra.clone());
    }
    let text = serde_json::to_string(&document).map_err(CliError::local)?;
    Ok(format!("{text}\n"))
}
