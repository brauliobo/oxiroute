use std::collections::HashMap;

use super::{Chip, Detail, ready};
use crate::cli::{
    top::{
        history::Sampling,
        model::{Cell, Column, Population, Row, Table, Tone},
        source::{Conn, ConnsDoc, Sample},
    },
    units,
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("ip", "CLIENT", 1),
    Column::text("listener", "LISTENER", 1),
    Column::text("proto", "PROTO", 2),
    Column::text("upstream", "UPSTREAM", 1),
    Column::text("state", "STATE", 1),
    Column::number("age", "AGE", 1),
    Column::number("idle", "IDLE", 2),
    Column::number("in", "IN/s", 1).summed(),
    Column::number("out", "OUT/s", 1).summed(),
    Column::number("intot", "IN tot", 2).summed(),
    Column::number("outtot", "OUT tot", 2).summed(),
    Column::text("request", "REQUEST", 2),
    Column::number("reqs", "REQS", 3).summed(),
    Column::text("service", "SERVICE", 3),
    Column::text("tls", "TLS", 4),
    Column::text("id", "ID", 3),
];

pub(super) fn table(sampling: &Sampling) -> Table {
    let document = match ready(&sampling.now.conns) {
        Ok(document) => document,
        Err(notice) => return Table::noticed(COLUMNS, notice),
    };
    let before = sampling.before.and_then(|before| before.conns.ready());
    let clock = sampling.feed_clock(
        document.sampled_at_unix_ms.0,
        before.map(|b| b.sampled_at_unix_ms.0),
    );
    let earlier = before
        .map(|before| {
            before
                .connections
                .iter()
                .map(|conn| (conn.id.0.as_str(), conn))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let at = document.sampled_at_unix_ms.0;
    let rows = document
        .connections
        .iter()
        .map(|conn| {
            let before = earlier.get(conn.id.0.as_str()).copied();
            row(conn, before, at, &clock)
        })
        .collect::<Vec<_>>();
    let shown = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    let mut table = Table::new(COLUMNS, rows).totalled();
    table.population = Some(Population {
        shown,
        of: document.total.0,
    });
    table.other = (document.total.0 > shown).then(|| {
        let hidden = format!("other ({})", units::count(document.total.0 - shown));
        Row::summary(
            &hidden,
            vec![Cell::text(&hidden)]
                .into_iter()
                .chain((1..COLUMNS.len()).map(|_| Cell::text("")))
                .collect(),
        )
    });
    table
}

fn state(conn: &Conn) -> Cell {
    let tone = match conn.state.as_str() {
        "handshake" | "closing" | "draining" => Tone::Warn,
        "idle" => Tone::Dim,
        _ => Tone::Plain,
    };
    Cell::text(&conn.state).toned(tone)
}

fn tls(conn: &Conn) -> String {
    conn.tls.as_ref().map_or_else(String::new, |tls| {
        [&tls.version, &tls.alpn, &tls.sni]
            .into_iter()
            .flatten()
            .map(|label| label.0.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn request(conn: &Conn) -> Cell {
    conn.current_request
        .as_ref()
        .map_or_else(Cell::missing, |request| {
            Cell::text(format!(
                "{} {} {}",
                request.method,
                request.path,
                units::millis(request.age_ms.0)
            ))
        })
}

fn row(
    conn: &Conn,
    before: Option<&Conn>,
    at: u64,
    clock: &crate::cli::top::history::Clock,
) -> Row {
    let born = Some(at.saturating_sub(conn.age_ms.0));
    let rate = |now: u64, earlier: Option<u64>| clock.rate(now, earlier, born);
    let peer = format!("{}:{}", conn.peer_ip, conn.peer_port.0);
    let upstream = conn.upstream.as_deref();
    let cells = vec![
        Cell::text(&peer),
        Cell::text(&conn.listener),
        Cell::text(&conn.protocol),
        Cell::optional_text(upstream),
        state(conn),
        Cell::duration(conn.age_ms.0),
        Cell::duration(conn.idle_ms.0),
        Cell::bytes_rate(rate(conn.bytes_in.0, before.map(|b| b.bytes_in.0))),
        Cell::bytes_rate(rate(conn.bytes_out.0, before.map(|b| b.bytes_out.0))),
        Cell::bytes(conn.bytes_in.0),
        Cell::bytes(conn.bytes_out.0),
        request(conn),
        Cell::count(conn.requests.0),
        Cell::optional_text(conn.service.as_deref()),
        Cell::text(tls(conn)),
        Cell::text(&conn.id.0),
    ];
    let names = [
        peer.as_str(),
        conn.listener.as_str(),
        conn.protocol.as_str(),
        upstream.unwrap_or_default(),
        conn.id.0.as_str(),
    ];
    Row::new(&conn.id.0, cells, &names)
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let Some(document) = sampling.now.conns.ready() else {
        return Vec::new();
    };
    let idle = document
        .connections
        .iter()
        .filter(|conn| conn.state == "idle")
        .count();
    let busy = document
        .connections
        .iter()
        .filter(|conn| conn.current_request.is_some())
        .count();
    let mut chips = vec![
        Chip::new("connections", units::count(document.total.0)),
        Chip::new("shown", document.connections.len().to_string()),
        Chip::new("serving", busy.to_string()),
        Chip::new("idle", idle.to_string()).toned(Tone::Dim),
    ];
    if document.truncated {
        chips.push(Chip::new("truncated", "yes").toned(Tone::Warn));
    }
    chips
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let Some(conn) =
        sampling.now.conns.ready().and_then(|document: &ConnsDoc| {
            document.connections.iter().find(|conn| conn.id.0 == key)
        })
    else {
        return Detail::default();
    };
    let mut detail = Detail::default()
        .with("id", &conn.id.0)
        .with("peer", format!("{}:{}", conn.peer_ip, conn.peer_port.0))
        .with("listener", &conn.listener)
        .with("protocol", &conn.protocol)
        .with("state", &conn.state)
        .with("age", units::duration(conn.age_ms.0))
        .with("idle", units::duration(conn.idle_ms.0))
        .with("requests", conn.requests.0.to_string())
        .with("bytes in", units::bytes(conn.bytes_in.0))
        .with("bytes out", units::bytes(conn.bytes_out.0));
    if let Some(service) = &conn.service {
        detail = detail.with("service", service);
    }
    if let Some(upstream) = &conn.upstream {
        detail = detail.with("upstream", upstream);
    }
    if let Some(tls) = &conn.tls {
        for (label, value) in [
            ("tls version", &tls.version),
            ("sni", &tls.sni),
            ("alpn", &tls.alpn),
        ] {
            if let Some(value) = value {
                detail = detail.with(label, &value.0);
            }
        }
    }
    detail
}

pub(super) fn badge(sample: &Sample) -> Option<(String, Tone)> {
    Some((units::count(sample.conns.ready()?.total.0), Tone::Dim))
}
