#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use oxiroute_h2::hpack::{Decoder, Encoder, Indexing};

const MAX_INPUT_BYTES: usize = 65_536;
const MAX_LIST_SIZE: usize = 16_384;
const MAX_FIELDS: usize = 256;
const BLOCK_FIELDS: usize = 8;
const TABLE_STEP: usize = 300;

type Fields = Vec<(Vec<u8>, Vec<u8>)>;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    if let Some(block) = support::strip_prefix(data, b"decode:") {
        decode_arbitrary(block);
    } else if let Some(spec) = support::strip_prefix(data, b"roundtrip:") {
        round_trip(spec);
    } else if let Some((&mode, rest)) = data.split_first() {
        if mode % 2 == 0 {
            decode_arbitrary(rest);
        } else {
            round_trip(rest);
        }
    }
});

/// Arbitrary bytes as header blocks: no panic, and a block that decodes stays within the list limit. The same
/// block is decoded twice so the second pass runs against the dynamic table the first one built.
fn decode_arbitrary(block: &[u8]) {
    for max_list in [64, MAX_LIST_SIZE, usize::MAX] {
        let mut decoder = Decoder::new(4096);
        for _ in 0..2 {
            let mut listed = 0usize;
            let outcome = decoder.decode(block, max_list, |name, value| {
                listed += name.len() + value.len() + 32
            });
            if outcome.is_ok() {
                assert!(listed <= max_list);
            }
        }
    }
}

/// Encoder and decoder of one direction with the block being built.
struct Session {
    encoder: Encoder,
    decoder: Decoder,
    block: Vec<u8>,
    expected: Fields,
}

impl Session {
    fn push(&mut self, name: &[u8], value: &[u8], indexing: Indexing) {
        self.encoder
            .encode_field(&mut self.block, name, value, indexing);
        self.expected.push((name.to_vec(), value.to_vec()));
        if self.expected.len() == BLOCK_FIELDS {
            self.flush();
        }
    }

    fn set_table_size(&mut self, size: usize) {
        self.flush();
        self.decoder.set_max_table_size(size);
        self.encoder.set_max_table_size(size);
    }

    /// Decodes the block built so far and compares it with the fields that went in; both tables must agree.
    fn flush(&mut self) {
        let mut decoded = Fields::new();
        let outcome = self
            .decoder
            .decode(&self.block, usize::MAX, |name, value| {
                decoded.push((name.to_vec(), value.to_vec()))
            })
            .expect("own encoder output decodes");
        assert!(!outcome.list_too_large);
        assert_eq!(decoded, self.expected);
        assert_eq!(self.encoder.table_size(), self.decoder.table_size());
        self.block.clear();
        self.expected.clear();
    }
}

/// The input describes fields: a control octet (indexing, table size change), name and value lengths, then the
/// octets. They are encoded in small blocks and decoded again.
fn round_trip(spec: &[u8]) {
    let mut session = Session {
        encoder: Encoder::new(8192),
        decoder: Decoder::new(8192),
        block: Vec::new(),
        expected: Fields::new(),
    };
    let mut rest = spec;
    let mut fields = 0;
    while let [control, name_len, value_len, tail @ ..] = rest {
        let (name_len, value_len) = (usize::from(*name_len), usize::from(*value_len) * 4);
        if tail.len() < name_len + value_len || fields == MAX_FIELDS {
            break;
        }
        let (name, tail) = tail.split_at(name_len);
        let (value, tail) = tail.split_at(value_len);
        rest = tail;
        fields += 1;
        if control & 0x40 != 0 {
            session.set_table_size(usize::from(control >> 2 & 0xf) * TABLE_STEP);
        }
        let indexing = match control & 3 {
            0 | 3 => Indexing::Incremental,
            1 => Indexing::None,
            _ => Indexing::Never,
        };
        session.push(name, value, indexing);
    }
    session.flush();
}
