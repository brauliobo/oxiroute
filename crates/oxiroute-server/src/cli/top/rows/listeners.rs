use super::{Cell, Column, Row, Tone};
use crate::{
    cli::{top::rates::Sampling, units},
    observability::stats_page::model::{
        AdministrativeState, Histogram, HttpOutcome, ListenerState, ListenerStats, TcpRelayOutcome,
    },
};

pub(super) fn columns() -> Vec<Column> {
    vec![
        Column::text("NAME"),
        Column::text("PROTO"),
        Column::text("BIND"),
        Column::text("STATE"),
        Column::number("CONN"),
        Column::number("ACC/s"),
        Column::number("REQ/s"),
        Column::number("IN/s"),
        Column::number("OUT/s"),
        Column::number("ERR/s"),
        Column::number("P50"),
        Column::number("P99"),
        Column::number("CACHE"),
    ]
}

pub(in crate::cli::top) fn rows(sampling: &Sampling) -> Vec<Row> {
    sampling
        .now
        .listeners
        .iter()
        .map(|listener| row(sampling, listener))
        .collect()
}

struct Operations<'a> {
    total: u64,
    errors: u64,
    latency: &'a Histogram,
}

fn operations(listener: &ListenerStats) -> Option<Operations<'_>> {
    if let Some(http) = &listener.http {
        let failed = |result| {
            matches!(
                result,
                HttpOutcome::ServerError
                    | HttpOutcome::UpstreamError
                    | HttpOutcome::Timeout
                    | HttpOutcome::InternalError
            )
        };
        let errors = http
            .outcomes
            .iter()
            .filter(|o| failed(o.result))
            .map(|o| o.count.get())
            .sum();
        return Some(Operations {
            total: http.total.get(),
            errors,
            latency: &http.latency,
        });
    }
    let relays = listener.tcp_relays.as_ref()?;
    let failed = |result| {
        matches!(
            result,
            TcpRelayOutcome::ConnectError
                | TcpRelayOutcome::ConnectTimeout
                | TcpRelayOutcome::IoError
                | TcpRelayOutcome::AccountingError
                | TcpRelayOutcome::ProxyProtocolError
        )
    };
    let errors = relays
        .outcomes
        .iter()
        .filter(|o| failed(o.result))
        .map(|o| o.count.get())
        .sum();
    Some(Operations {
        total: relays.total.get(),
        errors,
        latency: &relays.latency,
    })
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

fn connections(listener: &ListenerStats) -> Cell {
    let text = match listener.max_connections {
        Some(max) => format!("{}/{}", listener.active_connections, units::count(max)),
        None => listener.active_connections.to_string(),
    };
    Cell::number(text, units::as_f64(listener.active_connections))
}

fn row(sampling: &Sampling, listener: &ListenerStats) -> Row {
    let before = sampling.listener(&listener.name);
    let now = operations(listener);
    let earlier = before.and_then(operations);
    let [p50, p99] = now.as_ref().map_or_else(
        || [Cell::missing(), Cell::missing()],
        |now| sampling.percentiles(now.latency, earlier.as_ref().map(|earlier| earlier.latency)),
    );
    let rate = |count: fn(&Operations) -> u64| {
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
    Row::new(
        vec![
            Cell::text(&listener.name),
            Cell::text(&listener.protocol),
            Cell::text(&listener.bind),
            state(listener),
            connections(listener),
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
        ],
        &[&listener.name, &listener.protocol, &listener.bind],
    )
}
