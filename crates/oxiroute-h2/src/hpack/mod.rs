//! HPACK (RFC 7541): header compression for the HTTP/2 session.
//!
//! The API below is the contract the connection layer is written against. The implementation behind it is a
//! baseline taken from the research prototype and is meant to be replaced module by module.
#![allow(clippy::all, clippy::pedantic)]

mod baseline;
mod huff_table;
mod huffman;

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

pub struct Decoder {
    inner: baseline::Decoder,
    fields: baseline::Fields,
}

impl Decoder {
    /// `max_table_size` is the `SETTINGS_HEADER_TABLE_SIZE` this endpoint advertises (4096 by default): the
    /// encoder may signal any size up to it.
    #[must_use]
    pub fn new(max_table_size: usize) -> Self {
        Self {
            inner: baseline::Decoder::new(max_table_size),
            fields: baseline::Fields::default(),
        }
    }

    /// Decodes one complete header block (HEADERS plus CONTINUATION payloads, concatenated) and calls
    /// `visit(name, value)` for each field in wire order. Names and values are valid only during the call.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Malformed`] when the block is not valid HPACK.
    pub fn decode(
        &mut self,
        block: &[u8],
        max_list_size: usize,
        mut visit: impl FnMut(&[u8], &[u8]),
    ) -> Result<Decoded, DecodeError> {
        self.fields.clear();
        self.inner
            .decode(block, &mut self.fields)
            .map_err(|_| DecodeError::Malformed)?;
        let mut size = 0usize;
        let mut decoded = Decoded::default();
        for (name, value) in self.fields.iter() {
            size += name.len() + value.len() + 32;
            if size > max_list_size {
                decoded.list_too_large = true;
                break;
            }
            visit(name, value);
        }
        Ok(decoded)
    }
}

pub struct Encoder {
    inner: baseline::Encoder,
}

impl Encoder {
    /// `peer_max_table_size` is the peer's `SETTINGS_HEADER_TABLE_SIZE` (4096 until a SETTINGS frame says otherwise).
    #[must_use]
    pub fn new(peer_max_table_size: usize) -> Self {
        Self {
            inner: baseline::Encoder::new(peer_max_table_size),
        }
    }

    /// Appends one field. `name` must be lowercase.
    pub fn encode_field(
        &mut self,
        out: &mut Vec<u8>,
        name: &[u8],
        value: &[u8],
        indexing: Indexing,
    ) {
        let mode = match indexing {
            Indexing::Incremental => baseline::Mode::Index,
            Indexing::None => baseline::Mode::NoIndex,
            Indexing::Never => baseline::Mode::Never,
        };
        self.inner.field(out, name, value, mode);
    }
}
