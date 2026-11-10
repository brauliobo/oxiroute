//! HTTP/3 frames (RFC 9114 section 7): the type and length header, and the frame types this layer acts on.
mod header;
pub mod settings;

pub use header::HeaderReader;

pub const DATA: u64 = 0x0;
pub const HEADERS: u64 = 0x1;
pub const CANCEL_PUSH: u64 = 0x3;
pub const SETTINGS: u64 = 0x4;
pub const PUSH_PROMISE: u64 = 0x5;
pub const GOAWAY: u64 = 0x7;
pub const MAX_PUSH_ID: u64 = 0xd;

/// Unidirectional stream types (RFC 9114 section 6.2, RFC 9204 section 4.2).
pub mod stream_type {
    pub const CONTROL: u64 = 0x00;
    pub const PUSH: u64 = 0x01;
    pub const QPACK_ENCODER: u64 = 0x02;
    pub const QPACK_DECODER: u64 = 0x03;
}

/// Frame types of HTTP/2 that HTTP/3 reserves: receiving one is a connection error (RFC 9114 section 7.2.8).
#[must_use]
pub const fn is_reserved_h2(frame_type: u64) -> bool {
    matches!(frame_type, 0x2 | 0x6 | 0x8 | 0x9)
}

/// Appends a frame header.
pub fn write_header(out: &mut Vec<u8>, frame_type: u64, len: u64) {
    crate::varint::encode(out, frame_type);
    crate::varint::encode(out, len);
}
