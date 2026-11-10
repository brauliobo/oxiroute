//! Frame codec (RFC 9113 section 4 and 6): the 9-octet header, SETTINGS parsing and frame writers.
//!
//! Parsing of frame payloads that need connection state (HEADERS, DATA) lives with the state machine in
//! [`crate::conn`]; this module only knows the wire layout.
mod settings;
mod write;

pub use settings::{Settings, SettingsError};
pub use write::{
    data_head, go_away, headers_block, ping, put_head, rst_stream, settings_ack, settings_frame,
    window_update,
};

/// Length of the frame header.
pub const HEAD_LEN: usize = 9;
/// Client connection preface (RFC 9113 section 3.4).
pub const PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub const DEFAULT_MAX_FRAME_SIZE: u32 = 16_384;
pub const MAX_MAX_FRAME_SIZE: u32 = (1 << 24) - 1;
pub const DEFAULT_WINDOW: u32 = 65_535;
pub const MAX_WINDOW: u32 = (1 << 31) - 1;

pub const DATA: u8 = 0;
pub const HEADERS: u8 = 1;
pub const PRIORITY: u8 = 2;
pub const RST_STREAM: u8 = 3;
pub const SETTINGS: u8 = 4;
pub const PUSH_PROMISE: u8 = 5;
pub const PING: u8 = 6;
pub const GOAWAY: u8 = 7;
pub const WINDOW_UPDATE: u8 = 8;
pub const CONTINUATION: u8 = 9;

pub const END_STREAM: u8 = 0x1;
pub const ACK: u8 = 0x1;
pub const END_HEADERS: u8 = 0x4;
pub const PADDED: u8 = 0x8;
pub const PRIORITY_FLAG: u8 = 0x20;

/// Decoded frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub len: usize,
    pub kind: u8,
    pub flags: u8,
    pub stream: u32,
}

impl Head {
    /// Parses the first nine octets of `src`; the reserved bit of the stream id is dropped.
    ///
    /// # Panics
    ///
    /// When `src` is shorter than [`HEAD_LEN`].
    #[must_use]
    pub fn parse(src: &[u8]) -> Self {
        let h: &[u8; HEAD_LEN] = src[..HEAD_LEN].try_into().expect("nine octets");
        Self {
            len: usize::from(h[0]) << 16 | usize::from(h[1]) << 8 | usize::from(h[2]),
            kind: h[3],
            flags: h[4],
            stream: u32::from_be_bytes([h[5] & 0x7f, h[6], h[7], h[8]]),
        }
    }
}

/// Strips the PADDED framing from a payload: returns the data without the pad length octet and the padding.
///
/// # Errors
///
/// `Err(())` when the pad length is not smaller than the payload (a connection error `PROTOCOL_ERROR`).
#[allow(clippy::result_unit_err)]
pub fn unpad(payload: &[u8]) -> Result<&[u8], ()> {
    let (&pad, rest) = payload.split_first().ok_or(())?;
    rest.len()
        .checked_sub(usize::from(pad))
        .map(|n| &rest[..n])
        .ok_or(())
}
