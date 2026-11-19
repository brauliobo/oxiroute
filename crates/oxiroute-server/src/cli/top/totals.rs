use crate::observability::stats_page::model::{
    Histogram, HttpOutcome, ListenerStats, StatsSnapshot, TcpRelayOutcome,
};

pub(crate) struct Operations<'a> {
    pub(crate) total: u64,
    pub(crate) errors: u64,
    pub(crate) latency: &'a Histogram,
}

fn http_failed(result: HttpOutcome) -> bool {
    matches!(
        result,
        HttpOutcome::ServerError
            | HttpOutcome::UpstreamError
            | HttpOutcome::Timeout
            | HttpOutcome::InternalError
    )
}

fn relay_failed(result: TcpRelayOutcome) -> bool {
    matches!(
        result,
        TcpRelayOutcome::ConnectError
            | TcpRelayOutcome::ConnectTimeout
            | TcpRelayOutcome::IoError
            | TcpRelayOutcome::AccountingError
            | TcpRelayOutcome::ProxyProtocolError
    )
}

pub(crate) fn operations(listener: &ListenerStats) -> Option<Operations<'_>> {
    if let Some(http) = &listener.http {
        let errors = http
            .outcomes
            .iter()
            .filter(|outcome| http_failed(outcome.result))
            .map(|outcome| outcome.count.get())
            .sum();
        return Some(Operations {
            total: http.total.get(),
            errors,
            latency: &http.latency,
        });
    }
    let relays = listener.tcp_relays.as_ref()?;
    let errors = relays
        .outcomes
        .iter()
        .filter(|outcome| relay_failed(outcome.result))
        .map(|outcome| outcome.count.get())
        .sum();
    Some(Operations {
        total: relays.total.get(),
        errors,
        latency: &relays.latency,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Totals {
    pub(crate) requests: u64,
    pub(crate) errors: u64,
    pub(crate) accepted: u64,
    pub(crate) bytes_in: u64,
    pub(crate) bytes_out: u64,
}

impl Totals {
    pub(crate) fn of(snapshot: &StatsSnapshot) -> Self {
        let (requests, errors) = snapshot
            .listeners
            .iter()
            .filter_map(operations)
            .fold((0, 0), |(requests, errors), found| {
                (requests + found.total, errors + found.errors)
            });
        let process = &snapshot.process;
        Self {
            requests,
            errors,
            accepted: process.accepted_connections.get(),
            bytes_in: process.bytes_received.get(),
            bytes_out: process.bytes_sent.get(),
        }
    }
}
