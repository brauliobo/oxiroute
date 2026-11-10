//! HPACK (RFC 7541): header compression for the HTTP/2 session.
//!
//! [`Decoder`] and [`Encoder`] each own one direction's dynamic table. The dynamic table is a single byte arena
//! with absolute offsets ([`table`]), the static table is matched without hashing on the name bytes
//! ([`static_table`]), and Huffman decoding walks a compile-time trie, one lookup per symbol for common ASCII
//! ([`huffman`]).

// Wire-format code narrows masked or range-checked values to octets and words throughout.
#![allow(clippy::cast_possible_truncation)]

mod bytes_eq;
mod decoder;
mod encoder;
mod huff_table;
mod huffman;
mod integer;
mod static_table;
mod table;

pub use decoder::Decoder;
pub use encoder::Encoder;

/// Why a header block could not be decoded. Every variant is a connection error (`COMPRESSION_ERROR`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Malformed representation, bad index, bad Huffman data, integer overflow, or an illegal table size update.
    Malformed,
}

/// How a field is entered in (or kept out of) the peer's dynamic table when encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indexing {
    /// Literal with incremental indexing (or an indexed representation when the pair is already known).
    Incremental,
    /// Literal without indexing.
    None,
    /// Literal never indexed (secrets and unique values such as request ids).
    Never,
}

/// Result of decoding one block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    /// The decoded list (sum over fields of name + value + 32 octets, RFC 9113 section 6.5.2) went past the limit.
    /// The whole block was still processed so the dynamic table stays in step; `visit` is not called for the
    /// fields after the limit was crossed.
    pub list_too_large: bool,
}
