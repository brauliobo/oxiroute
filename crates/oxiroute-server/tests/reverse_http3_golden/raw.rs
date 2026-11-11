//! A raw quinn client that writes HTTP/3 frames by hand, to provoke protocol-level behaviour.

use std::{net::SocketAddr, time::Duration};

use h3::qpack::{HeaderField, decode_stateless, encode_stateless};
use serde_json::{Value, json};
use tokio::time::{Instant, timeout, timeout_at};

use crate::{client, golden};

pub const DATA: u64 = 0x0;
pub const HEADERS: u64 = 0x1;
pub const CANCEL_PUSH: u64 = 0x3;
pub const SETTINGS: u64 = 0x4;
pub const PUSH_PROMISE: u64 = 0x5;
pub const GOAWAY: u64 = 0x7;
pub const MAX_PUSH_ID: u64 = 0xd;
/// How long a case waits for an answer that may never come.
pub const SILENCE: Duration = Duration::from_millis(2500);
/// How long a case waits for a connection to be closed that may stay open.
pub const CLOSE_WAIT: Duration = Duration::from_millis(1000);
/// How long the end of a stream may precede the close of its connection.
const CLOSE_RACE: Duration = Duration::from_millis(300);

pub fn varint(out: &mut Vec<u8>, value: u64) {
    match value {
        0..=0x3f => out.push(value as u8),
        0x40..=0x3fff => out.extend_from_slice(&(0x4000 | value as u16).to_be_bytes()),
        0x4000..=0x3fff_ffff => out.extend_from_slice(&(0x8000_0000 | value as u32).to_be_bytes()),
        _ => out.extend_from_slice(&(0xc000_0000_0000_0000 | value).to_be_bytes()),
    }
}

pub fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, kind);
    varint(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

pub type Fields = Vec<(String, String)>;

/// The four request pseudo-header fields.
pub fn request(method: &str, host: &str, path: &str) -> Fields {
    [
        (":method", method),
        (":scheme", "https"),
        (":authority", host),
        (":path", path),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect()
}

pub fn base(host: &str, path: &str) -> Fields {
    request("GET", host, path)
}

pub fn with(mut fields: Fields, extra: &[(&str, &str)]) -> Fields {
    fields.extend(
        extra
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    fields
}

/// The QPACK field section of `fields` without dynamic table references.
pub fn section(fields: &Fields) -> Vec<u8> {
    let mut block = Vec::new();
    let fields = fields
        .iter()
        .map(|(name, value)| HeaderField::new(name.clone(), value.clone()));
    encode_stateless(&mut block, fields).expect("QPACK section");
    block
}

pub fn headers(fields: &Fields) -> Vec<u8> {
    frame(HEADERS, &section(fields))
}

/// The RFC 9114 size of a field section: every name and value plus 32 bytes per field.
pub fn section_size(fields: &Fields) -> usize {
    fields
        .iter()
        .map(|(name, value)| name.len() + value.len() + 32)
        .sum()
}

pub struct Raw {
    pub conn: quinn::Connection,
    pub control: Option<quinn::SendStream>,
}

impl Raw {
    /// A connection whose control stream opened with the given SETTINGS payload.
    pub async fn open_with(
        endpoint: &quinn::Endpoint,
        address: SocketAddr,
        settings: &[u8],
    ) -> Self {
        let conn = client::connect_quic(endpoint, address).await;
        let mut control = conn.open_uni().await.expect("control stream");
        let mut bytes = vec![0x00];
        bytes.extend(frame(SETTINGS, settings));
        control.write_all(&bytes).await.expect("SETTINGS");
        Self {
            conn,
            control: Some(control),
        }
    }

    pub async fn open(endpoint: &quinn::Endpoint, address: SocketAddr) -> Self {
        Self::open_with(endpoint, address, &[]).await
    }

    pub async fn without_control(endpoint: &quinn::Endpoint, address: SocketAddr) -> Self {
        Self {
            conn: client::connect_quic(endpoint, address).await,
            control: None,
        }
    }

    /// A control stream whose bytes after the stream type are `bytes`.
    pub async fn open_raw_control(
        endpoint: &quinn::Endpoint,
        address: SocketAddr,
        bytes: &[u8],
    ) -> Self {
        let conn = client::connect_quic(endpoint, address).await;
        let mut control = conn.open_uni().await.expect("control stream");
        control
            .write_all(&[&[0x00][..], bytes].concat())
            .await
            .expect("control bytes");
        Self {
            conn,
            control: Some(control),
        }
    }

    pub async fn uni(&self, kind: u64, bytes: &[u8]) -> quinn::SendStream {
        let mut stream = self.conn.open_uni().await.expect("unidirectional stream");
        let mut out = Vec::new();
        varint(&mut out, kind);
        out.extend_from_slice(bytes);
        stream
            .write_all(&out)
            .await
            .expect("unidirectional stream bytes");
        stream
    }

    /// Writes `bytes` on a new request stream, left open for the caller to finish.
    pub async fn request(&self, bytes: &[u8]) -> (quinn::SendStream, quinn::RecvStream) {
        let (mut send, recv) = self.conn.open_bi().await.expect("request stream");
        send.write_all(bytes).await.expect("request bytes");
        (send, recv)
    }

    /// Writes `bytes` on a new request stream and finishes it; the observed answer.
    ///
    /// A connection the server already closed (RFC 9114 mandates that for some violations) is the outcome.
    pub async fn exchange(&self, bytes: &[u8]) -> Value {
        let (mut send, mut recv) = match self.conn.open_bi().await {
            Ok(streams) => streams,
            Err(error) => return close_json(&error),
        };
        // a write that fails because the connection closed shows up as the close when the answer is read
        drop(send.write_all(bytes).await);
        drop(send.finish());
        self.settle(read_outcome(&mut recv, SILENCE).await).await
    }

    /// An answer that never came is explained by the connection closing, which can race with the stream ending.
    pub async fn settle(&self, outcome: Value) -> Value {
        if outcome.get("status").is_some()
            || outcome.get("silent").is_some()
            || outcome.get("connection_closed").is_some()
        {
            return outcome;
        }
        match timeout(CLOSE_RACE, self.conn.closed()).await {
            Ok(error) => close_json(&error),
            Err(_) => outcome,
        }
    }

    /// `{ "connection_closed": code }` once the peer closes the connection, or `{ "connection_open": true }`.
    pub async fn closed(&self, wait: Duration) -> Value {
        match timeout(wait, self.conn.closed()).await {
            Ok(error) => close_json(&error),
            Err(_) => json!({ "connection_open": true }),
        }
    }

    pub fn finish(self) {
        self.conn
            .close(quinn::VarInt::from_u32(0), b"golden case complete");
    }
}

pub fn close_json(error: &quinn::ConnectionError) -> Value {
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            json!({ "connection_closed": close.error_code.into_inner() })
        }
        quinn::ConnectionError::ConnectionClosed(close) => {
            json!({ "transport_closed": close.error_code.to_string() })
        }
        other => {
            json!({ "connection_lost": format!("{other:?}").split('(').next().unwrap_or_default() })
        }
    }
}

enum End {
    Fin,
    Reset(u64),
    Closed(Value),
    Silent,
}

/// Reads a response stream until it ends, is reset, the connection closes or `wait` passes.
pub async fn read_outcome(recv: &mut quinn::RecvStream, wait: Duration) -> Value {
    let deadline = Instant::now() + wait;
    let mut bytes = Vec::new();
    let end = loop {
        match timeout_at(deadline, recv.read_chunk(usize::MAX, true)).await {
            Ok(Ok(Some(chunk))) => bytes.extend_from_slice(&chunk.bytes),
            Ok(Ok(None)) => break End::Fin,
            Ok(Err(quinn::ReadError::Reset(code))) => break End::Reset(code.into_inner()),
            Ok(Err(quinn::ReadError::ConnectionLost(error))) => {
                break End::Closed(close_json(&error));
            }
            Ok(Err(_)) => break End::Closed(json!({ "read_error": true })),
            Err(_) => break End::Silent,
        }
    };
    let mut outcome = parse_response(&bytes).unwrap_or_else(|| json!({}));
    let answered = outcome.get("status").is_some();
    match end {
        End::Fin if !answered => outcome["finished_without_response"] = json!(true),
        End::Fin => {}
        End::Reset(code) => outcome["stream_reset"] = json!(code),
        End::Closed(close) => outcome
            .as_object_mut()
            .expect("outcome")
            .extend(close.as_object().cloned().unwrap_or_default()),
        End::Silent => outcome["silent"] = json!(true),
    }
    outcome
}

fn parse_response(mut bytes: &[u8]) -> Option<Value> {
    let (mut status, mut headers, mut trailers, mut body) = (None, Value::Null, None, Vec::new());
    while let Some((kind, payload, rest)) = split_frame(bytes) {
        bytes = rest;
        match kind {
            HEADERS if status.is_none() => (status, headers) = decode(payload),
            HEADERS => trailers = Some(decode(payload).1),
            DATA => body.extend_from_slice(payload),
            _ => {}
        }
    }
    let mut outcome =
        json!({ "status": status?, "headers": headers, "body": golden::body_json(&body) });
    if let Some(trailers) = trailers {
        outcome["trailers"] = trailers;
    }
    Some(outcome)
}

fn split_frame(bytes: &[u8]) -> Option<(u64, &[u8], &[u8])> {
    let (kind, rest) = read_varint(bytes)?;
    let (length, rest) = read_varint(rest)?;
    let length = usize::try_from(length).ok()?;
    (rest.len() >= length).then(|| (kind, &rest[..length], &rest[length..]))
}

fn read_varint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let width = 1usize << (bytes.first()? >> 6);
    let head = bytes.get(..width)?;
    let value = head.iter().enumerate().fold(0u64, |value, (index, byte)| {
        (value << 8) | u64::from(if index == 0 { byte & 0x3f } else { *byte })
    });
    Some((value, &bytes[width..]))
}

fn decode(payload: &[u8]) -> (Option<u16>, Value) {
    let mut buffer = payload;
    let decoded = decode_stateless(&mut buffer, u64::MAX).expect("QPACK response section");
    let status = decoded
        .fields
        .iter()
        .find(|field| field.name.as_ref() == b":status")
        .and_then(|field| std::str::from_utf8(&field.value).ok()?.parse().ok());
    let regular = decoded
        .fields
        .iter()
        .filter(|field| !field.name.starts_with(b":"));
    let headers = golden::headers_json(regular.map(|field| {
        (
            std::str::from_utf8(&field.name).expect("field name"),
            field.value.as_ref(),
        )
    }));
    (status, headers)
}
