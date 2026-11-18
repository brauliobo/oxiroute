use super::{
    Cell, Column, Row, Tone,
    traffic::{self, Counted},
};
use crate::{
    cli::top::{rates::Sampling, units},
    observability::stats_page::model::{ServerStats, ServerStatus},
};

pub(super) fn columns() -> Vec<Column> {
    let head = [
        Column::text("POOL"),
        Column::text("SERVER"),
        Column::text("STATUS"),
        Column::text("ADDRESS"),
        Column::number("CONN"),
        Column::number("WGT"),
    ];
    let tail = [Column::number("DOWN"), Column::number("CHKFAIL")];
    [
        head.as_slice(),
        traffic::COLUMNS.as_slice(),
        tail.as_slice(),
    ]
    .concat()
}

pub(in crate::cli::top) fn rows(sampling: &Sampling) -> Vec<Row> {
    sampling
        .now
        .pools
        .iter()
        .flat_map(|pool| {
            pool.servers
                .iter()
                .map(move |server| row(sampling, &pool.name, server))
        })
        .collect()
}

fn status(server: &ServerStats) -> Cell {
    let (text, tone) = match server.status {
        ServerStatus::Up => ("up", Tone::Good),
        ServerStatus::Down => ("down", Tone::Bad),
        ServerStatus::Drain => ("drain", Tone::Warn),
        ServerStatus::Maint => ("maint", Tone::Warn),
        ServerStatus::Ejected => ("ejected", Tone::Bad),
        ServerStatus::Unchecked => ("unchecked", Tone::Dim),
        ServerStatus::Unknown => ("unknown", Tone::Dim),
    };
    Cell::text(text).toned(tone)
}

fn connections(server: &ServerStats) -> Cell {
    let current = server.traffic.sessions.current;
    let text = match server.max_connections {
        Some(max) => format!("{current}/{}", units::count(max)),
        None => current.to_string(),
    };
    Cell::number(text, units::as_f64(current))
}

fn downtime(server: &ServerStats) -> Cell {
    server.downtime_ms.map_or_else(Cell::missing, |ms| {
        Cell::number(units::duration(ms), units::as_f64(ms)).toned(Tone::Bad)
    })
}

fn counted(server: &ServerStats) -> Counted<'_> {
    Counted {
        traffic: &server.traffic,
        latency: &server.latency,
    }
}

fn row(sampling: &Sampling, pool: &str, server: &ServerStats) -> Row {
    let mut cells = vec![
        Cell::text(pool),
        Cell::text(&server.name),
        status(server),
        Cell::text(&server.address),
        connections(server),
        Cell::number(server.weight.to_string(), f64::from(server.weight)),
    ];
    cells.extend(traffic::cells(
        sampling,
        &counted(server),
        sampling.server(pool, &server.name).map(counted),
    ));
    cells.extend([downtime(server), Cell::count(server.check.failed.get())]);
    Row::new(cells, &[pool, &server.name, &server.address])
}
