//! A raw quinn client that writes HTTP/3 bytes by hand, to provoke protocol violations.
#![allow(dead_code)]
use std::time::Duration;

use oxiroute_h3::frame::{self, settings};
use oxiroute_h3::varint;

pub fn frame_bytes(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    frame::write_header(&mut out, kind, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

pub fn varint_bytes(value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint::encode(&mut out, value);
    out
}

/// A request section from static references and literals: `GET https://example.com/` plus `extra` literal fields.
pub fn request_block(extra: &[(&str, &str)]) -> Vec<u8> {
    let mut block = vec![0, 0, 0xd1, 0xd7]; // :method GET, :scheme https
    block.extend_from_slice(&[0x50, 11]);
    block.extend_from_slice(b"example.com");
    block.extend_from_slice(&[0xc1]); // :path /
    for (name, value) in extra {
        oxiroute_h2::hpack::shared::integer_encode(&mut block, 0x20, 3, name.len());
        block.extend_from_slice(name.as_bytes());
        varint_prefix(&mut block, value.len());
        block.extend_from_slice(value.as_bytes());
    }
    block
}

fn varint_prefix(out: &mut Vec<u8>, len: usize) {
    oxiroute_h2::hpack::shared::integer_encode(out, 0, 7, len);
}

pub fn request_bytes(extra: &[(&str, &str)]) -> Vec<u8> {
    frame_bytes(frame::HEADERS, &request_block(extra))
}

pub struct Raw {
    pub conn: quinn::Connection,
    pub control: quinn::SendStream,
}

impl Raw {
    /// Opens the client control stream with a valid SETTINGS frame.
    pub async fn new(conn: quinn::Connection) -> Self {
        let mut control = conn.open_uni().await.unwrap();
        let mut bytes = vec![0x00];
        settings::encode(&mut bytes, 16_384);
        control.write_all(&bytes).await.unwrap();
        Self { conn, control }
    }

    pub async fn without_settings(conn: quinn::Connection) -> Self {
        let control = conn.open_uni().await.unwrap();
        Self { conn, control }
    }

    pub async fn uni(&self, kind: u64, bytes: &[u8]) -> quinn::SendStream {
        let mut stream = self.conn.open_uni().await.unwrap();
        stream.write_all(&varint_bytes(kind)).await.unwrap();
        stream.write_all(bytes).await.unwrap();
        stream
    }

    /// Sends `bytes` on a new request stream and finishes it.
    pub async fn request(&self, bytes: &[u8]) -> (quinn::SendStream, quinn::RecvStream) {
        let (mut send, recv) = self.conn.open_bi().await.unwrap();
        send.write_all(bytes).await.unwrap();
        (send, recv)
    }

    pub async fn closed_with(&self) -> u64 {
        let error = tokio::time::timeout(Duration::from_secs(30), self.conn.closed())
            .await
            .expect("not closed");
        match error {
            quinn::ConnectionError::ApplicationClosed(close) => u64::from(close.error_code),
            other => panic!("closed by {other:?}"),
        }
    }
}

/// Reads a response stream to its end: the frames as `(type, payload)`.
pub async fn read_frames(
    recv: &mut quinn::RecvStream,
) -> Result<Vec<(u64, Vec<u8>)>, quinn::ReadError> {
    let bytes = recv
        .read_to_end(1 << 24)
        .await
        .map_err(|error| match error {
            quinn::ReadToEndError::Read(error) => error,
            quinn::ReadToEndError::TooLong => panic!("response too long"),
        })?;
    let mut src = &bytes[..];
    let mut frames = Vec::new();
    while !src.is_empty() {
        let kind = varint::decode(&mut src).unwrap();
        let len = varint::decode(&mut src).unwrap() as usize;
        frames.push((kind, src[..len].to_vec()));
        src = &src[len..];
    }
    Ok(frames)
}

/// The `:status` of a response `HEADERS` payload that uses the encodings of this crate.
pub fn status_of(block: &[u8]) -> u16 {
    let mut section = oxiroute_h3::qpack::decode::section(block).unwrap();
    use oxiroute_h3::qpack::{decode::Line, static_table::ENTRIES};
    loop {
        match section.next_line().unwrap().expect("no :status") {
            Line::Indexed(i) if ENTRIES[usize::from(i)].0 == ":status" => {
                return ENTRIES[usize::from(i)].1.parse().unwrap();
            }
            Line::NameRef { index, value } if ENTRIES[usize::from(index)].0 == ":status" => {
                let mut out = Vec::new();
                value.write_to(&mut out).unwrap();
                return String::from_utf8(out).unwrap().parse().unwrap();
            }
            _ => {}
        }
    }
}
