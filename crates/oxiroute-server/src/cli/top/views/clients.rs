use std::collections::HashMap;

use super::{Chip, Detail, ready};
use crate::cli::{
    top::{
        history::{Clock, Sampling},
        model::{Cell, Column, Population, Row, Table, Tone},
        source::{Aggregate, ClientRow, ClientsDoc, Sample},
    },
    units,
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("ip", "CLIENT", 1),
    Column::number("conn", "CONN", 1).summed(),
    Column::number("new", "NEW/s", 2).summed(),
    Column::number("req", "REQ/s", 1).summed(),
    Column::number("in", "IN/s", 1).summed(),
    Column::number("out", "OUT/s", 1).summed(),
    Column::number("intot", "IN tot", 2).summed(),
    Column::number("outtot", "OUT tot", 2).summed(),
    Column::number("err", "ERR%", 1),
    Column::number("errors", "ERRORS", 3).summed(),
    Column::number("reqs", "REQS", 3).summed(),
    Column::number("age", "AGE", 2),
    Column::number("idle", "IDLE", 3),
    Column::text("listener", "LISTENER", 3),
    Column::text("listeners", "LISTENERS", 4),
    Column::text("proto", "PROTO", 3),
    Column::number("conns", "CONNS", 4).summed(),
];

pub(super) fn table(sampling: &Sampling) -> Table {
    let document = match ready(&sampling.now.clients) {
        Ok(document) => document,
        Err(notice) => return Table::noticed(COLUMNS, notice),
    };
    let before = sampling.before.and_then(|before| before.clients.ready());
    let clock = sampling.feed_clock(
        document.sampled_at_unix_ms.0,
        before.map(|b| b.sampled_at_unix_ms.0),
    );
    let earlier = before
        .map(|before| {
            before
                .clients
                .iter()
                .map(|client| (client.ip.as_str(), client))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let rows = document
        .clients
        .iter()
        .map(|client| {
            row(
                client,
                earlier.get(client.ip.as_str()).copied(),
                document,
                &clock,
            )
        })
        .collect();
    let mut table = Table::new(COLUMNS, rows);
    table.population = Some(Population {
        shown: u64::try_from(document.clients.len()).unwrap_or(u64::MAX),
        of: document.tracked_clients.0,
    });
    let others = before.map(|before| before.other);
    table.other = (document.other.clients.0 > 0 || document.truncated).then(|| {
        aggregate_row(
            &format!("other ({})", units::count(document.other.clients.0)),
            document.other,
            others,
            &clock,
        )
    });
    table.total = Some(aggregate_row(
        "TOTAL",
        document.totals,
        before.map(|before| before.totals),
        &clock,
    ));
    table
}

fn error_share(client: &ClientRow, before: Option<&ClientRow>, clock: &Clock) -> Cell {
    let errors = clock.change(client.errors.0, before.map(|before| before.errors.0));
    let requests = clock.change(client.requests.0, before.map(|before| before.requests.0));
    let share = errors
        .zip(requests)
        .and_then(|(errors, requests)| units::percent(errors, requests));
    let tone = match share {
        Some(share) if share > 5.0 => Tone::Bad,
        Some(share) if share > 1.0 => Tone::Warn,
        _ => Tone::Plain,
    };
    Cell::percent(share).toned(tone)
}

fn row(
    client: &ClientRow,
    before: Option<&ClientRow>,
    document: &ClientsDoc,
    clock: &Clock,
) -> Row {
    let born = Some(client.first_seen_unix_ms.0);
    let rate = |now: u64, earlier: Option<u64>| clock.rate(now, earlier, born);
    let at = document.sampled_at_unix_ms.0;
    let listener = client.listeners.first().cloned().unwrap_or_default();
    let cells = vec![
        Cell::text(&client.ip),
        Cell::plain(client.connections.0),
        Cell::rate(rate(
            client.connections_total.0,
            before.map(|b| b.connections_total.0),
        )),
        Cell::rate(rate(client.requests.0, before.map(|b| b.requests.0))),
        Cell::bytes_rate(rate(client.bytes_in.0, before.map(|b| b.bytes_in.0))),
        Cell::bytes_rate(rate(client.bytes_out.0, before.map(|b| b.bytes_out.0))),
        Cell::bytes(client.bytes_in.0),
        Cell::bytes(client.bytes_out.0),
        error_share(client, before, clock),
        Cell::count(client.errors.0).toned(if client.errors.0 > 0 {
            Tone::Warn
        } else {
            Tone::Plain
        }),
        Cell::count(client.requests.0),
        Cell::duration(at.saturating_sub(client.first_seen_unix_ms.0)),
        Cell::duration(at.saturating_sub(client.last_activity_unix_ms.0)),
        Cell::text(listener),
        Cell::text(client.listeners.join(",")),
        Cell::text(client.protocols.join(",")),
        Cell::count(client.connections_total.0),
    ];
    let search = client
        .listeners
        .iter()
        .chain(&client.protocols)
        .map(String::as_str);
    let names = std::iter::once(client.ip.as_str())
        .chain(search)
        .collect::<Vec<_>>();
    Row::new(&client.ip, cells, &names)
}

fn aggregate_row(label: &str, now: Aggregate, before: Option<Aggregate>, clock: &Clock) -> Row {
    let rate = |now: u64, earlier: Option<u64>| clock.rate(now, earlier, None);
    let blank = || Cell::text("");
    let cells = vec![
        Cell::text(label),
        Cell::plain(now.connections.0),
        blank(),
        Cell::rate(rate(now.requests.0, before.map(|b| b.requests.0))),
        Cell::bytes_rate(rate(now.bytes_in.0, before.map(|b| b.bytes_in.0))),
        Cell::bytes_rate(rate(now.bytes_out.0, before.map(|b| b.bytes_out.0))),
        Cell::bytes(now.bytes_in.0),
        Cell::bytes(now.bytes_out.0),
        blank(),
        blank(),
        Cell::count(now.requests.0),
        blank(),
        blank(),
        blank(),
        blank(),
        blank(),
        blank(),
    ];
    Row::new(label, cells, &[label])
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let Some(document) = sampling.now.clients.ready() else {
        return Vec::new();
    };
    let prefix = document
        .ipv6_prefix
        .map_or_else(String::new, |prefix| format!("/{}", prefix.0));
    let mut chips = vec![
        Chip::new("tracked", units::count(document.tracked_clients.0)),
        Chip::new("shown", document.clients.len().to_string()),
        Chip::new("connections", units::count(document.totals.connections.0)),
        Chip::new("in", units::bytes(document.totals.bytes_in.0)),
        Chip::new("out", units::bytes(document.totals.bytes_out.0)),
    ];
    if !prefix.is_empty() {
        chips.push(Chip::new("ipv6 grouped", prefix).toned(Tone::Dim));
    }
    if document.truncated {
        chips.push(Chip::new("truncated", "yes").toned(Tone::Warn));
    }
    chips
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let Some(client) = sampling
        .now
        .clients
        .ready()
        .and_then(|document| document.clients.iter().find(|client| client.ip == key))
    else {
        return Detail::default();
    };
    Detail::default()
        .with("client", &client.ip)
        .with("first seen", units::timestamp(client.first_seen_unix_ms.0))
        .with(
            "last activity",
            units::timestamp(client.last_activity_unix_ms.0),
        )
        .with(
            "connections",
            format!(
                "{} live, {} total",
                client.connections.0, client.connections_total.0
            ),
        )
        .with("requests", client.requests.0.to_string())
        .with("errors", client.errors.0.to_string())
        .with("bytes in", units::bytes(client.bytes_in.0))
        .with("bytes out", units::bytes(client.bytes_out.0))
        .with("listeners", client.listeners.join(", "))
        .with("protocols", client.protocols.join(", "))
}

pub(super) fn badge(sample: &Sample) -> Option<(String, Tone)> {
    let tracked = sample.clients.ready()?.tracked_clients.0;
    Some((units::count(tracked), Tone::Dim))
}
