use super::{
    Chip, Detail,
    traffic::{self, Cells, Counted},
    word,
};
use crate::{
    cli::top::{
        history::Sampling,
        model::{Cell, Column, Row, Table, Tone},
        source::Sample,
    },
    observability::stats_page::model::{
        HealthState, PoolStats, PoolStatus, ServerStats, ServerStatus,
    },
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("name", "NAME", 1),
    Column::text("state", "STATE", 1),
    Column::number("up", "UP", 2),
    Column::number("conn", "CONN", 1).summed(),
    Column::number("queue", "QUEUE", 1).summed(),
    Column::number("req", "REQ/s", 1).summed(),
    Column::number("in", "IN/s", 1).summed(),
    Column::number("out", "OUT/s", 1).summed(),
    Column::number("err", "ERR/s", 1).summed(),
    Column::number("2xx", "2XX", 3).summed(),
    Column::number("3xx", "3XX", 4).summed(),
    Column::number("4xx", "4XX", 2).summed(),
    Column::number("5xx", "5XX", 2).summed(),
    Column::number("p50", "P50", 3),
    Column::number("p99", "P99", 1),
    Column::text("health", "HEALTH", 2),
    Column::text("outlier", "OUTLIER", 3),
    Column::text("algo", "ALGO", 3),
    Column::text("address", "ADDRESS", 2),
    Column::number("wgt", "WGT", 3),
    Column::number("qwait", "QWAIT", 3),
    Column::number("infl", "INFL", 3).summed(),
    Column::number("down", "DOWN", 4),
    Column::number("chkfail", "CHKFAIL", 4),
    Column::number("max", "MAX", 4),
];

pub(super) fn table(sampling: &Sampling) -> Table {
    let mut rows = Vec::new();
    for pool in &sampling.stats().pools {
        let parent = rows.len();
        rows.push(pool_row(sampling, pool));
        for server in &pool.servers {
            rows.push(server_row(sampling, &pool.name, server).under(parent, 1));
        }
    }
    Table::new(COLUMNS, rows).totalled()
}

fn assemble(
    label: &str,
    head: [Cell; 3],
    traffic: Cells,
    tail: Vec<Cell>,
    queue: Cell,
    key: &str,
    names: &[&str],
) -> Row {
    let [state, up, conn] = head;
    let [c2, c3, c4, c5] = traffic.classes;
    let [p50, p99] = traffic.percentiles;
    let mut cells = vec![
        Cell::text(label),
        state,
        up,
        conn,
        queue,
        traffic.requests,
        traffic.input,
        traffic.output,
        traffic.errors,
        c2,
        c3,
        c4,
        c5,
        p50,
        p99,
    ];
    cells.extend(tail);
    Row::new(key, cells, names)
}

fn pool_state(pool: &PoolStats) -> Cell {
    match pool.status {
        PoolStatus::Up => Cell::text("up").toned(Tone::Good),
        PoolStatus::Degraded => Cell::text("degraded").toned(Tone::Warn),
        PoolStatus::Down => Cell::text("down").toned(Tone::Bad),
        PoolStatus::Empty => Cell::text("empty").toned(Tone::Dim),
    }
}

fn pool_counted(pool: &PoolStats) -> Counted<'_> {
    Counted {
        traffic: &pool.traffic,
        latency: &pool.latency,
    }
}

fn pool_row(sampling: &Sampling, pool: &PoolStats) -> Row {
    let traffic = traffic::cells(
        sampling,
        &pool_counted(pool),
        sampling.pool(&pool.name).map(pool_counted),
    );
    let queue_wait = match pool.queue.current {
        0 => Cell::missing(),
        _ => Cell::millis(pool.queue.oldest_wait_ms),
    };
    let health = pool
        .canary
        .as_ref()
        .and_then(|canary| canary.result)
        .map_or_else(Cell::missing, |result| {
            Cell::text(format!("canary {}", word(&result)))
        });
    let up = Cell::limited(pool.available_servers, Some(pool.total_servers));
    let tail = vec![
        health,
        Cell::missing(),
        Cell::text(&pool.algorithm),
        Cell::missing(),
        Cell::missing(),
        queue_wait,
        Cell::plain(pool.inflight.requests),
        Cell::missing(),
        Cell::missing(),
        Cell::missing(),
    ];
    assemble(
        &pool.name,
        [
            pool_state(pool),
            up,
            Cell::plain(pool.traffic.sessions.current),
        ],
        traffic,
        tail,
        Cell::count(pool.queue.current),
        &pool.name,
        &[&pool.name, &pool.algorithm],
    )
}

fn server_state(server: &ServerStats) -> Cell {
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

fn server_health(server: &ServerStats) -> Cell {
    match (server.health, server.check.consecutive_failures) {
        (HealthState::Unchecked, _) => Cell::text("unchecked").toned(Tone::Dim),
        (HealthState::Unknown, _) => Cell::text("unknown").toned(Tone::Dim),
        (HealthState::Healthy, _) => Cell::text("ok").toned(Tone::Good),
        (HealthState::Unhealthy, failures) => {
            Cell::text(format!("failing x{failures}")).toned(Tone::Bad)
        }
    }
}

fn outlier(server: &ServerStats) -> Cell {
    let passive = &server.passive;
    if passive.ejected {
        let reason = passive
            .ejection_reason
            .map_or_else(String::new, |reason| format!(" {}", word(&reason)));
        return Cell::text(format!("ejected{reason}")).toned(Tone::Bad);
    }
    match passive.ejection_count.get() {
        0 => Cell::missing(),
        count => Cell::text(format!("{count} past")).toned(Tone::Dim),
    }
}

fn server_counted(server: &ServerStats) -> Counted<'_> {
    Counted {
        traffic: &server.traffic,
        latency: &server.latency,
    }
}

fn server_row(sampling: &Sampling, pool: &str, server: &ServerStats) -> Row {
    let traffic = traffic::cells(
        sampling,
        &server_counted(server),
        sampling.server(pool, &server.name).map(server_counted),
    );
    let current = server.traffic.sessions.current;
    let tail = vec![
        server_health(server),
        outlier(server),
        Cell::missing(),
        Cell::text(&server.address),
        Cell::plain(u64::from(server.weight)),
        Cell::missing(),
        Cell::missing(),
        server
            .downtime_ms
            .map_or_else(Cell::missing, |ms| Cell::duration(ms).toned(Tone::Bad)),
        Cell::count(server.check.failed.get()),
        server
            .max_connections
            .map_or_else(Cell::missing, Cell::count),
    ];
    assemble(
        &server.name,
        [
            server_state(server),
            Cell::missing(),
            Cell::limited(current, server.max_connections),
        ],
        traffic,
        tail,
        Cell::missing(),
        &format!("{pool}/{}", server.name),
        &[pool, &server.name, &server.address],
    )
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let pools = &sampling.stats().pools;
    let servers = pools.iter().flat_map(|pool| &pool.servers);
    let (mut up, mut down, mut other) = (0, 0, 0);
    for server in servers {
        match server.status {
            ServerStatus::Up => up += 1,
            ServerStatus::Down | ServerStatus::Ejected => down += 1,
            _ => other += 1,
        }
    }
    let queued: u64 = pools.iter().map(|pool| pool.queue.current).sum();
    vec![
        Chip::new("pools", pools.len().to_string()),
        Chip::new("servers up", up.to_string()),
        Chip::new("down", down.to_string()).toned(if down > 0 { Tone::Bad } else { Tone::Dim }),
        Chip::new("other", other.to_string()).toned(Tone::Dim),
        Chip::new("queued", queued.to_string()).toned(if queued > 0 {
            Tone::Warn
        } else {
            Tone::Dim
        }),
    ]
}

fn server_detail(pool: &str, server: &ServerStats) -> Detail {
    let (check, passive) = (&server.check, &server.passive);
    let mut detail = Detail::default()
        .with("pool", pool)
        .with("address", &server.address)
        .with("origin", word(&server.origin))
        .with("status", word(&server.status))
        .with("admin", word(&server.administrative_state))
        .with(
            "weight",
            format!(
                "{} (configured {})",
                server.weight, server.configured_weight
            ),
        )
        .with(
            "checks",
            format!(
                "{} ok, {} failed, {} in a row",
                check.successful.get(),
                check.failed.get(),
                check.consecutive_failures
            ),
        )
        .with(
            "ejections",
            format!(
                "{} (recovered {})",
                passive.ejection_count.get(),
                passive.recovery_count.get()
            ),
        );
    if let Some(reason) = check.last_failure {
        detail = detail.with("last check failure", word(&reason));
    }
    if let Some(reason) = passive.ejection_reason {
        detail = detail.with("ejection reason", word(&reason));
    }
    detail
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let pools = &sampling.stats().pools;
    if let Some((pool, name)) = key.split_once('/') {
        return pools
            .iter()
            .find(|candidate| candidate.name == pool)
            .and_then(|pool| {
                pool.servers
                    .iter()
                    .find(|server| server.name == name)
                    .map(|server| server_detail(&pool.name, server))
            })
            .unwrap_or_default();
    }
    pools
        .iter()
        .find(|pool| pool.name == key)
        .map_or_else(Detail::default, |pool| {
            Detail::default()
                .with("algorithm", &pool.algorithm)
                .with("status", word(&pool.status))
                .with(
                    "servers",
                    format!(
                        "{} available of {}",
                        pool.available_servers, pool.total_servers
                    ),
                )
                .with(
                    "queue",
                    format!(
                        "{} waiting, {} total, {} timeouts",
                        pool.queue.current,
                        pool.queue.total.get(),
                        pool.queue.timeouts.get()
                    ),
                )
                .with(
                    "unavailable selections",
                    pool.unavailable_selections.get().to_string(),
                )
                .with(
                    "fail-open selections",
                    pool.fail_open_selections.get().to_string(),
                )
        })
}

pub(super) fn badge(sample: &Sample) -> (String, Tone) {
    let pools = &sample.snapshot.pools;
    let down = pools
        .iter()
        .filter(|pool| pool.status != PoolStatus::Up)
        .count();
    if down > 0 {
        (format!("!{down}"), Tone::Bad)
    } else {
        (pools.len().to_string(), Tone::Dim)
    }
}
