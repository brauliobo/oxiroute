//! Frame writers: append wire frames to a `BytesMut`.
use bytes::{BufMut, BytesMut};

use crate::frame::{
    ACK, CONTINUATION, DATA, END_HEADERS, END_STREAM, GOAWAY, HEADERS, PING, RST_STREAM, SETTINGS,
    WINDOW_UPDATE,
};

/// Appends a frame header.
pub fn put_head(buf: &mut BytesMut, len: usize, kind: u8, flags: u8, stream: u32) {
    let l = (len as u32).to_be_bytes();
    buf.put_slice(&[l[1], l[2], l[3], kind, flags]);
    buf.put_u32(stream);
}

/// Header of a DATA frame carrying `len` octets.
pub fn data_head(buf: &mut BytesMut, len: usize, stream: u32, end_stream: bool) {
    put_head(
        buf,
        len,
        DATA,
        if end_stream { END_STREAM } else { 0 },
        stream,
    );
}

pub fn settings_frame(buf: &mut BytesMut, entries: &[(u16, u32)]) {
    put_head(buf, entries.len() * 6, SETTINGS, 0, 0);
    for &(id, value) in entries {
        buf.put_u16(id);
        buf.put_u32(value);
    }
}

pub fn settings_ack(buf: &mut BytesMut) {
    put_head(buf, 0, SETTINGS, ACK, 0);
}

pub fn ping(buf: &mut BytesMut, payload: [u8; 8], ack: bool) {
    put_head(buf, 8, PING, if ack { ACK } else { 0 }, 0);
    buf.put_slice(&payload);
}

pub fn go_away(buf: &mut BytesMut, last_stream: u32, code: u32) {
    put_head(buf, 8, GOAWAY, 0, 0);
    buf.put_u32(last_stream);
    buf.put_u32(code);
}

pub fn window_update(buf: &mut BytesMut, stream: u32, increment: u32) {
    put_head(buf, 4, WINDOW_UPDATE, 0, stream);
    buf.put_u32(increment);
}

pub fn rst_stream(buf: &mut BytesMut, stream: u32, code: u32) {
    put_head(buf, 4, RST_STREAM, 0, stream);
    buf.put_u32(code);
}

/// One HEADERS frame holding `block`, continued with CONTINUATION frames when it exceeds `max_frame`.
pub fn headers_block(
    buf: &mut BytesMut,
    stream: u32,
    block: &[u8],
    end_stream: bool,
    max_frame: usize,
) {
    let mut chunks = block.chunks(max_frame);
    let first = chunks.next().unwrap_or_default();
    let mut flags = if end_stream { END_STREAM } else { 0 };
    if block.len() <= max_frame {
        flags |= END_HEADERS;
    }
    put_head(buf, first.len(), HEADERS, flags, stream);
    buf.put_slice(first);
    while let Some(chunk) = chunks.next() {
        let last = if chunks.len() == 0 { END_HEADERS } else { 0 };
        put_head(buf, chunk.len(), CONTINUATION, last, stream);
        buf.put_slice(chunk);
    }
}
