//! A thin HTTP/3 server layer (RFC 9114) directly over `quinn` streams, with QPACK (RFC 9204) that needs no
//! dynamic table.
//!
//! * [`server::Connection`] opens the control stream, accepts request streams and reads the peer's control, QPACK
//!   and unknown streams while it waits for them.
//! * [`server::RequestStream`] is owned by whoever serves the request: head, body and trailers in, response out.
//! * [`qpack`], [`frame`], [`varint`], [`head`] and [`parse`] are the codecs and parsers, usable without I/O.
//!
//! # QPACK
//!
//! The server announces `SETTINGS_QPACK_MAX_TABLE_CAPACITY` 0 and `SETTINGS_QPACK_BLOCKED_STREAMS` 0, so it opens no
//! QPACK stream (RFC 9204 section 4.2) and every request section decodes from the static table and literals
//! (Huffman strings included). A section that needs the dynamic table (Required Insert Count above zero, dynamic or
//! post-base references) is a connection error `QPACK_DECOMPRESSION_FAILED`. On the peer's encoder stream only
//! `Set Dynamic Table Capacity 0` is legal, anything else is `QPACK_ENCODER_STREAM_ERROR`; the peer's decoder stream
//! may cancel streams, other instructions are `QPACK_DECODER_STREAM_ERROR`. Responses use static references and
//! literals, never Huffman coding.
//!
//! # Abuse limits
//!
//! Each limit is covered by a test in `tests/wire.rs` or in the unit tests of [`parse`].
//!
//! * Field sections: the decoded size (sum of name and value lengths) is counted while decoding and stops at
//!   [`server::Config::max_field_section_size`] ([`HeadError::FieldSectionTooLarge`], the application answers 431). The
//!   encoded `HEADERS` frame may be at most [`server::HEADERS_FRAME_FACTOR`] times that; a larger one is refused from
//!   its frame header, before any payload is read, by resetting the stream with `H3_EXCESSIVE_LOAD`. Trailers are held
//!   to the same limits.
//! * Buffering: a request stream holds at most one received chunk plus one `HEADERS` payload within the bound above;
//!   `DATA` is handed out as zero-copy slices, unknown frames are skipped without being stored, a frame header split
//!   across chunks needs 16 bytes. The peer's control stream holds at most a 4 KiB `SETTINGS` payload, QPACK streams
//!   hold no state beyond one integer, unknown unidirectional streams are abandoned at once, and at most eight
//!   streams whose type has not arrived are kept (further ones wait for QUIC stream credit).
//! * Rapid reset: request streams the peer resets, or stops reading, before the response was finished are counted
//!   when the request task next reads or writes the stream; more than [`server::Config::reset_flood_limit`] within
//!   [`server::Config::reset_flood_window`] closes the connection with `H3_EXCESSIVE_LOAD`. The defaults (1,000 in 10
//!   seconds) are far above what a browser leaving a page cancels, and far below an attack.
//! * Tasks and memory: the crate spawns one task per connection (the peer's unidirectional streams) and none per
//!   request; the number of request streams in flight is bounded by the QUIC
//!   stream limit the application configures, and the receive windows bound the data.

// Frame lengths and stream offsets are protocol-bounded variable-length integers (below 2^62) that are narrowed
// only after the buffer they describe was bounds-checked, and octets are produced from masked values.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

pub mod code;
mod error;
pub mod frame;
pub mod head;
pub mod parse;
pub mod qpack;
pub mod server;
pub mod varint;

pub use code::Code;
pub use error::{Error, HeadError};
