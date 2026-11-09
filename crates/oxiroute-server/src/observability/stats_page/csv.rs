//! HAProxy-style CSV: one row per listener (`FRONTEND`), pool (`BACKEND`) and server, under the
//! `HAProxy` column names for the values the proxy has. A value the proxy does not know is an empty
//! cell, never a zero.
//!
//! Mapping: `bin` and `bout` are bytes from and to the client for a listener and bytes to and from
//! the server for a pool or server; `econ` counts failed connects, `eresp` timeouts and protocol
//! errors, `wretr` status retries and rescues; `qtime`, `rtime` and `ttime` are lifetime averages in
//! milliseconds; `lastchg` and `downtime` are seconds, `downtime` being the current outage only.

use super::model::{
    Counter, Histogram, HttpOutcome, ListenerState, ListenerStats, PoolStats, PoolStatus,
    ServerStats, ServerStatus, StatsSnapshot, TrafficStats, wire_name,
};

const HEADER: &str = "# pxname,svname,qcur,scur,slim,stot,bin,bout,ereq,econ,eresp,wretr,status,weight,\
chkfail,lastchg,downtime,type,check_status,hrsp_1xx,hrsp_2xx,hrsp_3xx,hrsp_4xx,hrsp_5xx,hrsp_other,\
req_tot,qtime,rtime,ttime";

pub(super) fn render(snapshot: &StatsSnapshot) -> Vec<u8> {
    let mut out = String::from(HEADER);
    out.push('\n');
    for listener in &snapshot.listeners {
        Row::frontend(listener).write(&mut out);
    }
    for pool in &snapshot.pools {
        Row::backend(pool).write(&mut out);
        for server in &pool.servers {
            Row::server(&pool.name, server, snapshot.sampled_at_unix_ms).write(&mut out);
        }
    }
    out.into_bytes()
}

#[derive(Default)]
struct Row<'a> {
    pxname: &'a str,
    svname: &'a str,
    qcur: Option<u64>,
    scur: Option<u64>,
    slim: Option<u64>,
    stot: Option<u64>,
    bin: Option<u64>,
    bout: Option<u64>,
    ereq: Option<u64>,
    econ: Option<u64>,
    eresp: Option<u64>,
    wretr: Option<u64>,
    status: Option<String>,
    weight: Option<u64>,
    chkfail: Option<u64>,
    lastchg: Option<u64>,
    downtime: Option<u64>,
    kind: u8,
    check_status: Option<String>,
    hrsp: Option<[u64; 6]>,
    req_tot: Option<u64>,
    qtime: Option<u64>,
    rtime: Option<u64>,
    ttime: Option<u64>,
}

impl<'a> Row<'a> {
    fn frontend(listener: &'a ListenerStats) -> Self {
        Self {
            pxname: &listener.name,
            svname: "FRONTEND",
            scur: Some(listener.active_connections),
            slim: listener.max_connections,
            stot: Some(listener.accepted_connections.get()),
            bin: Some(listener.bytes_received.get()),
            bout: Some(listener.bytes_sent.get()),
            ereq: listener.http.as_ref().and_then(|http| {
                http.outcomes
                    .iter()
                    .find(|outcome| outcome.result == HttpOutcome::ClientError)
                    .map(|outcome| outcome.count.get())
            }),
            status: Some(
                match listener.state {
                    ListenerState::Listening => "OPEN",
                    ListenerState::Stopped => "STOP",
                    ListenerState::Failed => "FAILED",
                    ListenerState::Configured => "CONFIGURED",
                }
                .into(),
            ),
            req_tot: listener.http.as_ref().map(|http| http.total.get()),
            kind: 0,
            ..Self::default()
        }
    }

    fn backend(pool: &'a PoolStats) -> Self {
        let mut row = Self {
            pxname: &pool.name,
            svname: "BACKEND",
            qcur: Some(pool.queue.current),
            status: Some(
                match pool.status {
                    PoolStatus::Up | PoolStatus::Degraded => "UP",
                    PoolStatus::Down | PoolStatus::Empty => "DOWN",
                }
                .into(),
            ),
            qtime: average(&pool.queue.wait),
            kind: 1,
            ..Self::default()
        };
        row.traffic(&pool.traffic, &pool.latency.header, &pool.latency.request);
        row
    }

    fn server(pool: &'a str, server: &'a ServerStats, now_unix_ms: u64) -> Self {
        let mut row = Self {
            pxname: pool,
            svname: &server.name,
            slim: server.max_connections,
            status: Some(server_status(server.status).into()),
            weight: Some(u64::from(server.weight)),
            chkfail: Some(server.check.failed.get()),
            lastchg: server
                .last_transition_at_unix_ms
                .map(|at| now_unix_ms.saturating_sub(at) / 1_000),
            downtime: server.downtime_ms.map(|ms| ms / 1_000),
            check_status: server
                .check
                .last_checked_at_unix_ms
                .map(|_| {
                    server
                        .check
                        .last_failure
                        .filter(|_| server.check.consecutive_failures > 0)
                })
                .map(|failure| failure.map_or_else(|| "ok".into(), |failure| wire_name(&failure))),
            kind: 2,
            ..Self::default()
        };
        row.traffic(
            &server.traffic,
            &server.latency.header,
            &server.latency.request,
        );
        row
    }

    fn traffic(&mut self, traffic: &TrafficStats, header: &Histogram, request: &Histogram) {
        let attempts = &traffic.attempts;
        self.scur = Some(traffic.sessions.current);
        self.stot = Some(traffic.sessions.total.get());
        self.bin = Some(traffic.bytes_sent.get());
        self.bout = Some(traffic.bytes_received.get());
        self.econ = Some(attempts.connect_failed.get());
        self.eresp = Some(attempts.timeout.get() + attempts.protocol_error.get());
        self.wretr = Some(attempts.status_retry.get() + attempts.rescued.get());
        let responses = &traffic.responses;
        self.hrsp = Some(
            [
                responses.c1xx,
                responses.c2xx,
                responses.c3xx,
                responses.c4xx,
                responses.c5xx,
                responses.none,
            ]
            .map(Counter::get),
        );
        self.rtime = average(header);
        self.ttime = average(request);
    }

    fn write(&self, out: &mut String) {
        text(out, self.pxname);
        out.push(',');
        text(out, self.svname);
        for value in [
            self.qcur, self.scur, self.slim, self.stot, self.bin, self.bout, self.ereq, self.econ,
            self.eresp, self.wretr,
        ] {
            cell(out, value);
        }
        out.push(',');
        text(out, self.status.as_deref().unwrap_or_default());
        for value in [
            self.weight,
            self.chkfail,
            self.lastchg,
            self.downtime,
            Some(u64::from(self.kind)),
        ] {
            cell(out, value);
        }
        out.push(',');
        text(out, self.check_status.as_deref().unwrap_or_default());
        for index in 0..6 {
            cell(out, self.hrsp.map(|classes| classes[index]));
        }
        for value in [self.req_tot, self.qtime, self.rtime, self.ttime] {
            cell(out, value);
        }
        out.push('\n');
    }
}

fn server_status(status: ServerStatus) -> &'static str {
    match status {
        ServerStatus::Up => "UP",
        ServerStatus::Down => "DOWN",
        ServerStatus::Drain => "DRAIN",
        ServerStatus::Maint => "MAINT",
        ServerStatus::Ejected => "EJECTED",
        ServerStatus::Unchecked => "no check",
        ServerStatus::Unknown => "UNKNOWN",
    }
}

fn average(histogram: &Histogram) -> Option<u64> {
    histogram.sum_ms.get().checked_div(histogram.count.get())
}

fn cell(out: &mut String, value: Option<u64>) {
    out.push(',');
    if let Some(value) = value {
        out.push_str(&value.to_string());
    }
}

/// Quotes what CSV needs quoted and defuses what a spreadsheet would run as a formula.
fn text(out: &mut String, value: &str) {
    let quoted = value.contains([',', '"', '\n', '\r']);
    if quoted {
        out.push('"');
    }
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        out.push('\'');
    }
    out.push_str(&value.replace('"', "\"\""));
    if quoted {
        out.push('"');
    }
}
