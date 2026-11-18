use super::{
    Cell, Column, Row, Tone,
    traffic::{self, Counted},
};
use crate::{
    cli::{top::rates::Sampling, units},
    observability::stats_page::model::{PoolStats, PoolStatus},
};

pub(super) fn columns() -> Vec<Column> {
    let head = [
        Column::text("NAME"),
        Column::text("ALGO"),
        Column::text("STATUS"),
        Column::number("UP"),
        Column::number("QUEUE"),
        Column::number("QWAIT"),
        Column::number("INFL"),
    ];
    [head.as_slice(), traffic::COLUMNS.as_slice()].concat()
}

pub(in crate::cli::top) fn rows(sampling: &Sampling) -> Vec<Row> {
    sampling
        .now
        .pools
        .iter()
        .map(|pool| row(sampling, pool))
        .collect()
}

fn status(pool: &PoolStats) -> Cell {
    match pool.status {
        PoolStatus::Up => Cell::text("up").toned(Tone::Good),
        PoolStatus::Degraded => Cell::text("degraded").toned(Tone::Warn),
        PoolStatus::Down => Cell::text("down").toned(Tone::Bad),
        PoolStatus::Empty => Cell::text("empty").toned(Tone::Dim),
    }
}

fn queue_wait(pool: &PoolStats) -> Cell {
    match pool.queue.current {
        0 => Cell::missing(),
        _ => Cell::number(
            units::millis(pool.queue.oldest_wait_ms),
            units::as_f64(pool.queue.oldest_wait_ms),
        ),
    }
}

fn counted(pool: &PoolStats) -> Counted<'_> {
    Counted {
        traffic: &pool.traffic,
        latency: &pool.latency,
    }
}

fn row(sampling: &Sampling, pool: &PoolStats) -> Row {
    let available = Cell::number(
        format!("{}/{}", pool.available_servers, pool.total_servers),
        units::as_f64(pool.available_servers),
    );
    let mut cells = vec![
        Cell::text(&pool.name),
        Cell::text(&pool.algorithm),
        status(pool),
        available,
        Cell::count(pool.queue.current),
        queue_wait(pool),
        Cell::count(pool.inflight.requests),
    ];
    cells.extend(traffic::cells(
        sampling,
        &counted(pool),
        sampling.pool(&pool.name).map(counted),
    ));
    Row::new(cells, &[&pool.name, &pool.algorithm])
}
