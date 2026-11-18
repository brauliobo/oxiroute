use super::{Cell, Column, Tone};
use crate::{
    cli::top::rates::Sampling,
    observability::stats_page::model::{LatencyStats, TrafficStats},
};

pub(super) const COLUMNS: [Column; 10] = [
    Column::number("REQ/s"),
    Column::number("IN/s"),
    Column::number("OUT/s"),
    Column::number("2XX"),
    Column::number("3XX"),
    Column::number("4XX"),
    Column::number("5XX"),
    Column::number("ERR/s"),
    Column::number("P50"),
    Column::number("P99"),
];

#[derive(Clone, Copy)]
pub(super) struct Counted<'a> {
    pub(super) traffic: &'a TrafficStats,
    pub(super) latency: &'a LatencyStats,
}

fn attempt_errors(counted: &Counted) -> u64 {
    let attempts = &counted.traffic.attempts;
    attempts.connect_failed.get() + attempts.timeout.get() + attempts.protocol_error.get()
}

fn class(count: u64, tone: Tone) -> Cell {
    Cell::count(count).toned(if count > 0 { tone } else { Tone::Plain })
}

pub(super) fn cells(sampling: &Sampling, now: &Counted, before: Option<Counted>) -> Vec<Cell> {
    let traffic = now.traffic;
    let earlier = before.as_ref().map(|before| before.traffic);
    let classes = &traffic.responses;
    let [p50, p99] = sampling.percentiles(
        &now.latency.request,
        before.as_ref().map(|before| &before.latency.request),
    );
    vec![
        Cell::rate(sampling.rate(
            traffic.sessions.total.get(),
            earlier.map(|t| t.sessions.total.get()),
        )),
        Cell::bytes_rate(sampling.rate(
            traffic.bytes_sent.get(),
            earlier.map(|t| t.bytes_sent.get()),
        )),
        Cell::bytes_rate(sampling.rate(
            traffic.bytes_received.get(),
            earlier.map(|t| t.bytes_received.get()),
        )),
        Cell::count(classes.c2xx.get()),
        Cell::count(classes.c3xx.get()),
        class(classes.c4xx.get(), Tone::Warn),
        class(classes.c5xx.get(), Tone::Bad),
        Cell::rate(sampling.rate(attempt_errors(now), before.as_ref().map(attempt_errors))),
        p50,
        p99,
    ]
}
