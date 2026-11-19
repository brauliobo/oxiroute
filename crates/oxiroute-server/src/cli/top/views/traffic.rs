use crate::{
    cli::top::{
        history::Sampling,
        model::{Cell, Tone},
    },
    observability::stats_page::model::{LatencyStats, TrafficStats},
};

#[derive(Clone, Copy)]
pub(super) struct Counted<'a> {
    pub(super) traffic: &'a TrafficStats,
    pub(super) latency: &'a LatencyStats,
}

pub(super) struct Cells {
    pub(super) requests: Cell,
    pub(super) input: Cell,
    pub(super) output: Cell,
    pub(super) errors: Cell,
    pub(super) classes: [Cell; 4],
    pub(super) percentiles: [Cell; 2],
}

fn attempt_errors(counted: &Counted) -> u64 {
    let attempts = &counted.traffic.attempts;
    attempts.connect_failed.get() + attempts.timeout.get() + attempts.protocol_error.get()
}

fn class(count: u64, tone: Tone) -> Cell {
    Cell::count(count).toned(if count > 0 { tone } else { Tone::Plain })
}

pub(super) fn cells(sampling: &Sampling, now: &Counted, before: Option<Counted>) -> Cells {
    let traffic = now.traffic;
    let earlier = before.as_ref().map(|before| before.traffic);
    let classes = &traffic.responses;
    Cells {
        requests: Cell::rate(sampling.rate(
            traffic.sessions.total.get(),
            earlier.map(|t| t.sessions.total.get()),
        )),
        input: Cell::bytes_rate(sampling.rate(
            traffic.bytes_sent.get(),
            earlier.map(|t| t.bytes_sent.get()),
        )),
        output: Cell::bytes_rate(sampling.rate(
            traffic.bytes_received.get(),
            earlier.map(|t| t.bytes_received.get()),
        )),
        errors: Cell::rate(sampling.rate(attempt_errors(now), before.as_ref().map(attempt_errors))),
        classes: [
            Cell::count(classes.c2xx.get()),
            Cell::count(classes.c3xx.get()),
            class(classes.c4xx.get(), Tone::Warn),
            class(classes.c5xx.get(), Tone::Bad),
        ],
        percentiles: sampling.percentiles(
            &now.latency.request,
            before.as_ref().map(|before| &before.latency.request),
        ),
    }
}
