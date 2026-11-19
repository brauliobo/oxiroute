use super::{Chip, Detail, ready};
use crate::cli::{
    top::{
        history::Sampling,
        model::{Cell, Column, Population, Row, Table, Tone},
        source::{Request, Sample},
    },
    units,
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("client", "CLIENT", 1),
    Column::text("method", "METHOD", 1),
    Column::text("path", "PATH", 1),
    Column::text("phase", "PHASE", 1),
    Column::number("age", "AGE", 1),
    Column::text("host", "HOST", 2),
    Column::text("listener", "LISTENER", 2),
    Column::text("pool", "POOL", 2),
    Column::text("server", "SERVER", 2),
    Column::number("wait", "WAIT", 2),
    Column::number("try", "TRY", 3),
    Column::text("id", "ID", 3),
];

pub(super) fn table(sampling: &Sampling) -> Table {
    let requests = match ready(&sampling.now.requests) {
        Ok(requests) => requests,
        Err(notice) => return Table::noticed(COLUMNS, notice),
    };
    let rows = requests.items.iter().map(row).collect::<Vec<_>>();
    let shown = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    let mut table = Table::new(COLUMNS, rows);
    table.population = Some(Population {
        shown,
        of: requests.total.0,
    });
    table
}

fn row(request: &Request) -> Row {
    let wait = request
        .waited_ms
        .map_or_else(Cell::missing, |wait| Cell::millis(wait.0));
    let tone = if request.overdue {
        Tone::Bad
    } else {
        Tone::Plain
    };
    let cells = vec![
        Cell::optional_text(request.client_ip.as_deref()),
        Cell::text(&request.method),
        Cell::text(&request.path),
        Cell::text(&request.phase),
        Cell::millis(request.age_ms.0).toned(tone),
        Cell::optional_text(request.host.as_deref()),
        Cell::text(&request.listener),
        Cell::optional_text(request.pool.as_deref()),
        Cell::optional_text(request.server.as_deref()),
        wait,
        Cell::plain(u64::from(request.attempts)),
        Cell::text(&request.id),
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
    Row::new(&request.id, cells, &names)
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let Some(requests) = sampling.now.requests.ready() else {
        return Vec::new();
    };
    let overdue = requests
        .items
        .iter()
        .filter(|request| request.overdue)
        .count();
    let mut chips = vec![
        Chip::new("in flight", units::count(requests.total.0)),
        Chip::new("shown", requests.items.len().to_string()),
        Chip::new("overdue", overdue.to_string()).toned(if overdue > 0 {
            Tone::Bad
        } else {
            Tone::Dim
        }),
    ];
    if requests.truncated {
        chips.push(Chip::new("truncated", "oldest first").toned(Tone::Warn));
    }
    chips
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let Some(request) = sampling
        .now
        .requests
        .ready()
        .and_then(|requests| requests.items.iter().find(|request| request.id == key))
    else {
        return Detail::default();
    };
    let mut detail = Detail::default()
        .with("id", &request.id)
        .with("listener", &request.listener)
        .with("request", format!("{} {}", request.method, request.path))
        .with("phase", &request.phase)
        .with("age", units::millis(request.age_ms.0))
        .with("attempts", request.attempts.to_string())
        .with("overdue", if request.overdue { "yes" } else { "no" });
    for (label, value) in [
        ("client", &request.client_ip),
        ("host", &request.host),
        ("route", &request.route),
        ("pool", &request.pool),
        ("server", &request.server),
    ] {
        if let Some(value) = value {
            detail = detail.with(label, value);
        }
    }
    detail
}

pub(super) fn badge(sample: &Sample) -> Option<(String, Tone)> {
    let requests = sample.requests.ready()?;
    let overdue = requests.items.iter().any(|request| request.overdue);
    Some((
        units::count(requests.total.0),
        if overdue { Tone::Bad } else { Tone::Dim },
    ))
}
