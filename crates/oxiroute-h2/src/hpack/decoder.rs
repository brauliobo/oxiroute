//! Header block decoding (RFC 7541 section 3 and 6).
use super::huffman::{self, Count};
use super::table::{MAX_SIZE, Table};
use super::{DecodeError, Decoded, integer, static_table};
use std::ops::Range;

/// At most two size updates may open a block: the smallest size since the last block, then the final one.
const MAX_SIZE_UPDATES: usize = 2;
/// Scratch space above this is released after a block.
const KEEP_SCRATCH: usize = 16 * 1024;
const OVERHEAD: usize = super::table::ENTRY_OVERHEAD;

/// A name or value: borrowed from the block or the static table, decoded or copied into the scratch buffer, or
/// only measured when nothing needs its bytes.
enum Text<'a> {
    Borrowed(&'a [u8]),
    Scratch(Range<usize>),
    Unstored,
}

/// Decoding half of a connection: owns the dynamic table the peer's encoder builds.
#[derive(Debug)]
pub struct Decoder {
    table: Table,
    /// Largest table size the peer may announce.
    limit: usize,
    /// The limit dropped below the table's maximum: the next block must announce a size.
    must_update: bool,
    scratch: Vec<u8>,
}

/// Running total of the decoded list (RFC 9113 section 6.5.2).
struct List {
    size: usize,
    max: usize,
    too_large: bool,
}

impl List {
    #[inline]
    fn push(&mut self, visit: &mut impl FnMut(&[u8], &[u8]), name: &[u8], value: &[u8]) {
        self.size = self
            .size
            .saturating_add(name.len() + value.len() + OVERHEAD);
        if self.size > self.max {
            self.too_large = true;
        } else {
            visit(name, value);
        }
    }
}

impl Decoder {
    /// `max_table_size` is the `SETTINGS_HEADER_TABLE_SIZE` this endpoint advertises (4096 by default): the
    /// encoder may signal any size up to it.
    #[must_use]
    pub fn new(max_table_size: usize) -> Self {
        let limit = max_table_size.min(MAX_SIZE);
        Self {
            table: Table::new(limit),
            limit,
            must_update: false,
            scratch: Vec::new(),
        }
    }

    /// Changes the largest table size the peer may announce, after this endpoint sent a new
    /// `SETTINGS_HEADER_TABLE_SIZE`. When it is lower than the table's current maximum the next block must open
    /// with a size update.
    pub fn set_max_table_size(&mut self, max_table_size: usize) {
        self.limit = max_table_size.min(MAX_SIZE);
        self.must_update = self.table.max() > self.limit;
    }

    /// Current size of the dynamic table in the sense of RFC 7541 section 4.1 (entries count 32 octets extra).
    #[must_use]
    pub fn table_size(&self) -> usize {
        self.table.size()
    }

    /// Decodes one complete header block (HEADERS plus CONTINUATION payloads, concatenated) and calls
    /// `visit(name, value)` for each field in wire order. Names and values are valid only during the call.
    ///
    /// The whole block is always processed so the dynamic table stays in step with the peer's encoder, but once
    /// the decoded list passes `max_list_size` `visit` is no longer called and nothing more is copied for the list.
    /// Work and memory are proportional to the block, never to the size of the table entries it refers to.
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
        let mut src = block;
        let mut list = List {
            size: 0,
            max: max_list_size,
            too_large: false,
        };
        self.size_updates(&mut src)?;
        while let Some(&first) = src.first() {
            if first & 0x80 != 0 {
                self.indexed(&mut src, &mut list, &mut visit)?;
            } else if first & 0xe0 == 0x20 {
                return Err(DecodeError::Malformed);
            } else {
                self.literal(&mut src, first, &mut list, &mut visit)?;
            }
        }
        if self.scratch.capacity() > KEEP_SCRATCH {
            self.scratch = Vec::new();
        }
        Ok(Decoded {
            list_too_large: list.too_large,
        })
    }

    fn size_updates(&mut self, src: &mut &[u8]) -> Result<(), DecodeError> {
        let mut updates = 0;
        while src.first().is_some_and(|&b| b & 0xe0 == 0x20) {
            updates += 1;
            let size = integer::decode(src, 5).ok_or(DecodeError::Malformed)?;
            if updates > MAX_SIZE_UPDATES || size > self.limit {
                return Err(DecodeError::Malformed);
            }
            self.table.set_max(size);
        }
        if self.must_update && updates == 0 {
            return Err(DecodeError::Malformed);
        }
        self.must_update = false;
        Ok(())
    }

    #[inline]
    fn indexed(
        &self,
        src: &mut &[u8],
        list: &mut List,
        visit: &mut impl FnMut(&[u8], &[u8]),
    ) -> Result<(), DecodeError> {
        let index = integer::decode(src, 7).ok_or(DecodeError::Malformed)?;
        let (name, value) = self.entry(index).ok_or(DecodeError::Malformed)?;
        list.push(visit, name, value);
        Ok(())
    }

    #[inline]
    fn entry(&self, index: usize) -> Option<(&[u8], &[u8])> {
        if index > static_table::LEN {
            self.table.get(index - static_table::LEN)
        } else {
            static_table::get(index)
        }
    }

    fn literal(
        &mut self,
        src: &mut &[u8],
        first: u8,
        list: &mut List,
        visit: &mut impl FnMut(&[u8], &[u8]),
    ) -> Result<(), DecodeError> {
        let indexed = first & 0x40 != 0;
        let name_index =
            integer::decode(src, if indexed { 6 } else { 4 }).ok_or(DecodeError::Malformed)?;
        let store = indexed || !list.too_large;
        let cap = list.max.max(self.table.max());
        self.scratch.clear();
        if name_index > static_table::LEN && !indexed {
            return self.without_indexing(src, name_index, list, visit, (store, cap));
        }
        let name = match name_index {
            0 => read_text(&mut self.scratch, src, store, cap)?,
            index if index <= static_table::LEN => {
                Text::Borrowed(static_table::get(index).ok_or(DecodeError::Malformed)?.0)
            }
            index => self.copy_name(index)?,
        };
        let value = read_text(&mut self.scratch, src, store, cap)?;
        let (Some(name), Some(value)) = (
            resolve(&self.scratch, &name),
            resolve(&self.scratch, &value),
        ) else {
            list.too_large = true;
            if indexed {
                self.table.clear();
            }
            return Ok(());
        };
        list.push(visit, name, value);
        if indexed {
            self.table.insert(name, value);
        }
        Ok(())
    }

    /// A dynamic table name with a value that is not inserted: the name is lent to `visit` straight from the table.
    fn without_indexing(
        &mut self,
        src: &mut &[u8],
        index: usize,
        list: &mut List,
        visit: &mut impl FnMut(&[u8], &[u8]),
        (store, cap): (bool, usize),
    ) -> Result<(), DecodeError> {
        let (name, _) = self
            .table
            .get(index - static_table::LEN)
            .ok_or(DecodeError::Malformed)?;
        let value = read_text(&mut self.scratch, src, store, cap)?;
        match resolve(&self.scratch, &value) {
            Some(value) => list.push(visit, name, value),
            None => list.too_large = true,
        }
        Ok(())
    }

    /// Copies a dynamic table name out of reach of the insert that follows, which may evict its entry.
    fn copy_name(&mut self, index: usize) -> Result<Text<'static>, DecodeError> {
        let (name, _) = self
            .table
            .get(index - static_table::LEN)
            .ok_or(DecodeError::Malformed)?;
        let start = self.scratch.len();
        self.scratch.extend_from_slice(name);
        Ok(Text::Scratch(start..self.scratch.len()))
    }
}

fn resolve<'s>(scratch: &'s [u8], text: &'s Text<'_>) -> Option<&'s [u8]> {
    match text {
        Text::Borrowed(bytes) => Some(bytes),
        Text::Scratch(range) => scratch.get(range.clone()),
        Text::Unstored => None,
    }
}

/// Reads one string literal. Raw strings are borrowed. Huffman strings are always validated; their bytes are
/// stored in `scratch` only when `store` is set and they fit `cap`, so a block never makes the decoder keep more
/// than the larger of the list limit and the table size.
fn read_text<'a>(
    scratch: &mut Vec<u8>,
    src: &mut &'a [u8],
    store: bool,
    cap: usize,
) -> Result<Text<'a>, DecodeError> {
    let huffman = src.first().ok_or(DecodeError::Malformed)? & 0x80 != 0;
    let len = integer::decode(src, 7).ok_or(DecodeError::Malformed)?;
    let (raw, rest) = src.split_at_checked(len).ok_or(DecodeError::Malformed)?;
    *src = rest;
    if !huffman {
        return Ok(Text::Borrowed(raw));
    }
    let fits = store && huffman::max_decoded_len(len) <= cap || {
        let mut count = Count(0);
        huffman::decode(raw, &mut count).map_err(|_| DecodeError::Malformed)?;
        store && count.0 <= cap
    };
    if !fits {
        return Ok(Text::Unstored);
    }
    let start = scratch.len();
    scratch.reserve(huffman::max_decoded_len(len));
    huffman::decode(raw, scratch).map_err(|_| DecodeError::Malformed)?;
    Ok(Text::Scratch(start..scratch.len()))
}
