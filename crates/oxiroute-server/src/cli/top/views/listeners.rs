use super::{Chip, Detail, word};
use crate::{
    cli::{
        top::{
            history::Sampling,
            model::{Cell, Column, Row, Table, Tone},
            source::Sample,
            totals::operations,
        },
        units,
    },
    observability::stats_page::model::{AdministrativeState, ListenerState, ListenerStats},
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("name", "NAME", 1),
    Column::text("proto", "PROTO", 3),
    Column::text("bind", "BIND", 2),
    Column::text("state", "STATE", 1),
    Column::number("conn", "CONN", 1).summed(),
    Column::number("acc", "ACC/s", 2).summed(),
    Column::number("req", "REQ/s", 1).summed(),
    Column::number("in", "IN/s", 1).summed(),
    Column::number("out", "OUT/s", 1).summed(),
    Column::number("err", "ERR/s", 1).summed(),
    Column::number("p50", "P50", 3),
    Column::number("p99", "P99", 1),
    Column::number("cache", "CACHE", 3),
    Column::number("max", "MAX", 4),
];

pub(super) fn table(sampling: &Sampling) -> Table {
    let rows = sampling
        .stats()
        .listeners
        .iter()
        .map(|listener| row(sampling, listener))
        .collect();
    Table::new(COLUMNS, rows).totalled()
}

fn healthy(listener: &ListenerStats) -> bool {
    listener.administrative_state == AdministrativeState::Ready
        && listener.state == ListenerState::Listening
}

fn state(listener: &ListenerStats) -> Cell {
    match (listener.administrative_state, listener.state) {
        (AdministrativeState::Drain, _) => Cell::text("drain").toned(Tone::Warn),
        (AdministrativeState::Maintenance, _) => Cell::text("maint").toned(Tone::Warn),
        (_, ListenerState::Listening) => Cell::text("up").toned(Tone::Good),
        (_, ListenerState::Configured) => Cell::text("conf").toned(Tone::Dim),
        (_, ListenerState::Stopped) => Cell::text("stopped").toned(Tone::Warn),
        (_, ListenerState::Failed) => Cell::text("failed").toned(Tone::Bad),
    }
}

fn row(sampling: &Sampling, listener: &ListenerStats) -> Row {
    let before = sampling.listener(&listener.name);
    let now = operations(listener);
    let earlier = before.and_then(operations);
    let [p50, p99] = now.as_ref().map_or_else(
        || [Cell::missing(), Cell::missing()],
        |now| sampling.percentiles(now.latency, earlier.as_ref().map(|earlier| earlier.latency)),
    );
    let rate = |count: fn(&crate::cli::top::totals::Operations) -> u64| {
        let now = now.as_ref()?;
        sampling.rate(count(now), earlier.as_ref().map(count))
    };
    let cache = listener.cache.as_ref().map_or_else(Cell::missing, |cache| {
        let earlier = before.and_then(|before| before.cache.as_ref());
        sampling.ratio(
            (cache.hits.get(), cache.misses.get()),
            earlier.map(|cache| (cache.hits.get(), cache.misses.get())),
        )
    });
    let cells = vec![
        Cell::text(&listener.name),
        Cell::text(&listener.protocol),
        Cell::text(&listener.bind),
        state(listener),
        Cell::limited(listener.active_connections, listener.max_connections),
        Cell::rate(sampling.rate(
            listener.accepted_connections.get(),
            before.map(|b| b.accepted_connections.get()),
        )),
        Cell::rate(rate(|operations| operations.total)),
        Cell::bytes_rate(sampling.rate(
            listener.bytes_received.get(),
            before.map(|b| b.bytes_received.get()),
        )),
        Cell::bytes_rate(sampling.rate(
            listener.bytes_sent.get(),
            before.map(|b| b.bytes_sent.get()),
        )),
        Cell::rate(rate(|operations| operations.errors)),
        p50,
        p99,
        cache,
        listener
            .max_connections
            .map_or_else(Cell::missing, Cell::count),
    ];
    Row::new(
        &listener.name,
        cells,
        &[&listener.name, &listener.protocol, &listener.bind],
    )
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let listeners = &sampling.stats().listeners;
    let unhealthy = listeners
        .iter()
        .filter(|listener| !healthy(listener))
        .count();
    let process = &sampling.stats().process;
    vec![
        Chip::new("listeners", listeners.len().to_string()),
        Chip::new("healthy", (listeners.len() - unhealthy).to_string()),
        Chip::new("unhealthy", unhealthy.to_string()).toned(if unhealthy > 0 {
            Tone::Warn
        } else {
            Tone::Dim
        }),
        Chip::new("accepted", units::count(process.accepted_connections.get())),
        Chip::new("rejected", units::count(process.rejected_connections.get())).toned(
            if process.rejected_connections.get() > 0 {
                Tone::Warn
            } else {
                Tone::Dim
            },
        ),
    ]
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let Some(listener) = sampling
        .stats()
        .listeners
        .iter()
        .find(|listener| listener.name == key)
    else {
        return Detail::default();
    };
    let mut detail = Detail::default()
        .with("bind", &listener.bind)
        .with("protocol", &listener.protocol)
        .with("state", word(&listener.state))
        .with("admin", word(&listener.administrative_state))
        .with(
            "accepted",
            units::count(listener.accepted_connections.get()),
        )
        .with(
            "rejected",
            units::count(listener.rejected_connections.get()),
        )
        .with("bytes in", units::bytes(listener.bytes_received.get()))
        .with("bytes out", units::bytes(listener.bytes_sent.get()));
    for (label, value) in [
        ("service", &listener.service),
        ("tls profile", &listener.tls_profile),
        ("pool", &listener.upstream_pool),
    ] {
        if let Some(value) = value {
            detail = detail.with(label, value);
        }
    }
    if let Some(http) = &listener.http {
        for outcome in &http.outcomes {
            detail = detail.with(
                "http",
                format!("{} {}", word(&outcome.result), outcome.count.get()),
            );
        }
    }
    if let Some(cache) = &listener.cache {
        detail = detail.with(
            "cache",
            format!("{} hits, {} misses", cache.hits.get(), cache.misses.get()),
        );
    }
    detail
}

pub(super) fn badge(sample: &Sample) -> (String, Tone) {
    let listeners = &sample.snapshot.listeners;
    let unhealthy = listeners
        .iter()
        .filter(|listener| !healthy(listener))
        .count();
    if unhealthy > 0 {
        (format!("!{unhealthy}"), Tone::Warn)
    } else {
        (listeners.len().to_string(), Tone::Dim)
    }
}
