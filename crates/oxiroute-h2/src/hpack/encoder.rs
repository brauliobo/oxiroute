//! Header block encoding (RFC 7541 section 6).
use super::table::Table;
use super::{Indexing, huffman, integer, static_table};

/// The encoder keeps at most this much of the peer's table: lookups scan the table, and a larger one buys
/// little for the fields a proxy sends. When the peer allows more, the first block announces this size.
const TABLE_SIZE: usize = 4096;
/// Strings up to this many octets have a one-octet length, so they are Huffman coded in place.
const SHORT: usize = 126;

pub struct Encoder {
    table: Table,
    /// Smallest table size set since the last block, to be announced before the current one.
    smallest: Option<usize>,
}

impl Encoder {
    /// `peer_max_table_size` is the peer's `SETTINGS_HEADER_TABLE_SIZE` (4096 until a SETTINGS frame says otherwise).
    #[must_use]
    pub fn new(peer_max_table_size: usize) -> Self {
        let size = peer_max_table_size.min(TABLE_SIZE);
        Self {
            table: Table::new(size),
            smallest: (size < peer_max_table_size).then_some(size),
        }
    }

    /// Applies a new `SETTINGS_HEADER_TABLE_SIZE` from the peer. The next field encoded opens a block that
    /// announces the change (the smallest size since the previous block, then the final one, RFC 7541 4.2).
    pub fn set_max_table_size(&mut self, peer_max_table_size: usize) {
        let size = peer_max_table_size.min(TABLE_SIZE);
        if size != self.table.max() || self.smallest.is_some() {
            self.smallest = Some(self.smallest.map_or(size, |smallest| smallest.min(size)));
            self.table.set_max(size);
        }
    }

    /// Current size of the peer's dynamic table as this encoder tracks it (RFC 7541 section 4.1).
    #[must_use]
    pub fn table_size(&self) -> usize {
        self.table.size()
    }

    /// Appends one field. `name` must be lowercase.
    pub fn encode_field(
        &mut self,
        out: &mut Vec<u8>,
        name: &[u8],
        value: &[u8],
        indexing: Indexing,
    ) {
        if let Some(smallest) = self.smallest.take() {
            self.announce_size(out, smallest);
        }
        let found = self.table.find(name, value);
        if let (Some(n), true) = (found.full, indexing != Indexing::Never) {
            return integer::encode(out, 0x80, 7, static_table::LEN + n);
        }
        let named = static_table::find(name);
        let index = named.and_then(|named| named.with_value(value));
        if let (Some(index), true) = (index, indexing != Indexing::Never) {
            return integer::encode(out, 0x80, 7, index);
        }
        let name_index = named
            .map(|named| named.first)
            .or_else(|| found.name.map(|n| static_table::LEN + n));
        self.literal(out, name_index, name, value, indexing);
    }

    fn announce_size(&self, out: &mut Vec<u8>, smallest: usize) {
        if smallest < self.table.max() {
            integer::encode(out, 0x20, 5, smallest);
        }
        integer::encode(out, 0x20, 5, self.table.max());
    }

    fn literal(
        &mut self,
        out: &mut Vec<u8>,
        name_index: Option<usize>,
        name: &[u8],
        value: &[u8],
        indexing: Indexing,
    ) {
        let (pattern, prefix) = match indexing {
            Indexing::Incremental => (0x40, 6),
            Indexing::None => (0x00, 4),
            Indexing::Never => (0x10, 4),
        };
        integer::encode(out, pattern, prefix, name_index.unwrap_or(0));
        if name_index.is_none() {
            put_string(out, name);
        }
        put_string(out, value);
        if indexing == Indexing::Incremental {
            self.table.insert(name, value);
        }
    }
}

/// String literal: Huffman coded when that is shorter.
fn put_string(out: &mut Vec<u8>, src: &[u8]) {
    if src.len() > SHORT {
        return put_long_string(out, src);
    }
    let start = out.len();
    out.push(0);
    huffman::encode(src, out);
    let coded = out.len() - start - 1;
    if coded < src.len() {
        out[start] = 0x80 | coded as u8;
    } else {
        out.truncate(start + 1);
        out[start] = src.len() as u8;
        out.extend_from_slice(src);
    }
}

fn put_long_string(out: &mut Vec<u8>, src: &[u8]) {
    let coded = huffman::encoded_len(src);
    if coded < src.len() {
        integer::encode(out, 0x80, 7, coded);
        huffman::encode(src, out);
    } else {
        integer::encode(out, 0, 7, src.len());
        out.extend_from_slice(src);
    }
}
