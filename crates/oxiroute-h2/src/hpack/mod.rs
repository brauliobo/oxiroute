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

/// Primitives the HTTP/3 QPACK codec shares with HPACK (RFC 9204 uses the RFC 7541 Huffman code and integers).
pub mod shared {
    /// Bytes the Huffman form of `src` takes.
    #[must_use]
    #[inline]
    pub fn huffman_encoded_len(src: &[u8]) -> usize {
        super::huffman::encoded_len(src)
    }

    /// Appends the Huffman form of `src`.
    #[inline]
    pub fn huffman_encode(src: &[u8], out: &mut Vec<u8>) {
        super::huffman::encode(src, out);
    }

    /// Appends the decoded form of `src`, or `false` when it is not valid Huffman data.
    #[inline]
    pub fn huffman_decode(src: &[u8], out: &mut Vec<u8>) -> bool {
        super::huffman::decode(src, out).is_ok()
    }

    /// Reads an integer with a `prefix`-bit prefix, `None` when truncated or past 32 bits.
    #[inline]
    pub fn integer_decode(src: &mut &[u8], prefix: u32) -> Option<usize> {
        super::integer::decode(src, prefix)
    }

    /// Appends `value` with a `prefix`-bit prefix whose high bits are `first`.
    #[inline]
    pub fn integer_encode(out: &mut Vec<u8>, first: u8, prefix: u32, value: usize) {
        super::integer::encode(out, first, prefix, value);
    }
}

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
    /// Literal without indexing (an indexed representation when the pair is already in a table, which adds nothing).
    None,
    /// Literal never indexed (secrets and unique values such as request ids), always a literal.
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
