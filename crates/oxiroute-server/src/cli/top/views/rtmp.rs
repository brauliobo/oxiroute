use std::collections::HashMap;

use super::{Chip, Detail, ready};
use crate::cli::{
    top::{
        history::Sampling,
        model::{Cell, Column, Row, Table, Tone},
        source::{RtmpDoc, Sample, Session, Stream},
    },
    units,
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("name", "NAME", 1),
    Column::text("role", "ROLE", 1),
    Column::number("subs", "SUBS", 1).summed(),
    Column::number("in", "IN/s", 1).summed(),
    Column::number("audio", "AUDIO/s", 3).summed(),
    Column::number("video", "VIDEO/s", 3).summed(),
    Column::number("bytes", "BYTES", 2).summed(),
    Column::text("peer", "PEER", 2),
    Column::text("service", "SERVICE", 3),
    Column::number("age", "AGE", 2),
];

pub(super) fn table(sampling: &Sampling) -> Table {
    let document = match ready(&sampling.now.rtmp) {
        Ok(document) => document,
        Err(notice) => return Table::noticed(COLUMNS, notice),
    };
    let before = sampling.before.and_then(|before| before.rtmp.ready());
    let clock = sampling.feed_clock(document.as_of_unix_ms.0, before.map(|b| b.as_of_unix_ms.0));
    let earlier = before
        .map(|before| {
            before
                .live
                .iter()
                .map(|stream| (stream.id.as_str(), stream))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let at = document.as_of_unix_ms.0;
    let mut rows = Vec::new();
    let mut parents = HashMap::new();
    for stream in &document.live {
        parents.insert(
            stream_key((&stream.service, &stream.application, &stream.name)),
            rows.len(),
        );
        let rates = [
            (
                stream.audio_payload_bytes.0,
                earlier
                    .get(stream.id.as_str())
                    .map(|s| s.audio_payload_bytes.0),
            ),
            (
                stream.video_payload_bytes.0,
                earlier
                    .get(stream.id.as_str())
                    .map(|s| s.video_payload_bytes.0),
            ),
        ]
        .map(|(now, before)| clock.rate(now, before, Some(stream.created_at_unix_ms.0)));
        rows.push(stream_row(stream, rates, at));
    }
    for session in &document.clients {
        let parent = session_stream(session).and_then(|key| parents.get(&key).copied());
        let row = session_row(session, at);
        rows.push(match parent {
            Some(parent) => row.under(parent, 1),
            None => row,
        });
    }
    Table::new(COLUMNS, rows).totalled()
}

fn stream_key((service, application, name): (&str, &str, &str)) -> String {
    format!("{service}/{application}/{name}")
}

fn session_stream(session: &Session) -> Option<String> {
    Some(stream_key((
        &session.service,
        session.application.as_deref()?,
        session.stream.as_deref()?,
    )))
}

fn sum(rates: &[Option<f64>; 2]) -> Option<f64> {
    rates[0].zip(rates[1]).map(|(audio, video)| audio + video)
}

fn stream_row(stream: &Stream, rates: [Option<f64>; 2], at: u64) -> Row {
    let name = format!("{}/{}", stream.application, stream.name);
    let bytes = stream.audio_payload_bytes.0 + stream.video_payload_bytes.0;
    let cells = vec![
        Cell::text(&name),
        Cell::text("stream").toned(Tone::Good),
        Cell::plain(stream.subscriber_count.0),
        Cell::bytes_rate(sum(&rates)),
        Cell::bytes_rate(rates[0]),
        Cell::bytes_rate(rates[1]),
        Cell::bytes(bytes),
        Cell::text(""),
        Cell::text(&stream.service),
        Cell::duration(at.saturating_sub(stream.created_at_unix_ms.0)),
    ];
    let key = stream_key((&stream.service, &stream.application, &stream.name));
    Row::new(key, cells, &[&name, &stream.service])
}

fn session_row(session: &Session, at: u64) -> Row {
    let peer = session.peer_ip.as_deref();
    let name = match (&session.application, &session.stream) {
        (Some(application), Some(stream)) => format!("{application}/{stream}"),
        _ => "(connecting)".to_owned(),
    };
    let cells = vec![
        Cell::text(peer.unwrap_or(&session.id)),
        Cell::text(&session.role),
        Cell::missing(),
        Cell::missing(),
        Cell::missing(),
        Cell::missing(),
        Cell::missing(),
        Cell::optional_text(peer),
        Cell::text(&session.service),
        Cell::duration(at.saturating_sub(session.connected_at_unix_ms.0)),
    ];
    Row::new(
        format!("session:{}", session.id),
        cells,
        &[&name, peer.unwrap_or_default(), &session.service],
    )
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let Some(document) = sampling.now.rtmp.ready() else {
        return Vec::new();
    };
    let global = &document.global;
    let mut chips = vec![
        Chip::new("streams", global.active_streams.0.to_string()),
        Chip::new("publishers", global.publishers.0.to_string()),
        Chip::new("viewers", global.subscribers.0.to_string()),
        Chip::new("audio", units::bytes(global.audio_payload_bytes.0)),
        Chip::new("video", units::bytes(global.video_payload_bytes.0)),
    ];
    if document.live_truncated || document.clients_truncated {
        chips.push(Chip::new("truncated", "yes").toned(Tone::Warn));
    }
    chips
}

fn stream_detail(document: &RtmpDoc, key: &str) -> Detail {
    document
        .live
        .iter()
        .find(|stream| stream_key((&stream.service, &stream.application, &stream.name)) == key)
        .map_or_else(Detail::default, |stream| {
            Detail::default()
                .with("stream", format!("{}/{}", stream.application, stream.name))
                .with("service", &stream.service)
                .with("subscribers", stream.subscriber_count.0.to_string())
                .with("audio bytes", units::bytes(stream.audio_payload_bytes.0))
                .with("video bytes", units::bytes(stream.video_payload_bytes.0))
                .with("started", units::timestamp(stream.created_at_unix_ms.0))
        })
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let Some(document) = sampling.now.rtmp.ready() else {
        return Detail::default();
    };
    let Some(id) = key.strip_prefix("session:") else {
        return stream_detail(document, key);
    };
    document
        .clients
        .iter()
        .find(|session| session.id == id)
        .map_or_else(Detail::default, |session| {
            let mut detail = Detail::default()
                .with("session", &session.id)
                .with("role", &session.role)
                .with("service", &session.service)
                .with(
                    "connected",
                    units::timestamp(session.connected_at_unix_ms.0),
                );
            for (label, value) in [
                ("peer", &session.peer_ip),
                ("application", &session.application),
                ("stream", &session.stream),
            ] {
                if let Some(value) = value {
                    detail = detail.with(label, value);
                }
            }
            detail
        })
}

pub(super) fn badge(sample: &Sample) -> Option<(String, Tone)> {
    Some((sample.rtmp.ready()?.live.len().to_string(), Tone::Dim))
}
