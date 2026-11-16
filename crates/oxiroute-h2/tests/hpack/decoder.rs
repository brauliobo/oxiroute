//! Decoder behaviour on malformed and hostile blocks, table size updates, eviction and the list size limit.
use oxiroute_h2::hpack::{DecodeError, Decoded, Decoder, Encoder, Indexing};

type Fields = Vec<(Vec<u8>, Vec<u8>)>;

fn decode_with(
    dec: &mut Decoder,
    block: &[u8],
    max_list: usize,
) -> Result<(Decoded, Fields), DecodeError> {
    let mut out = Vec::new();
    let decoded = dec.decode(block, max_list, |n, v| out.push((n.to_vec(), v.to_vec())))?;
    Ok((decoded, out))
}

fn decode(dec: &mut Decoder, block: &[u8]) -> Result<Fields, DecodeError> {
    decode_with(dec, block, usize::MAX).map(|(_, fields)| fields)
}

fn pair(name: &str, value: &str) -> (Vec<u8>, Vec<u8>) {
    (name.into(), value.into())
}

/// A raw string: 7-bit prefix length, then the octets.
fn string(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut len = text.len();
    if len < 127 {
        out.push(u8::try_from(len).unwrap());
    } else {
        out.push(0x7f);
        len -= 127;
        while len >= 128 {
            out.push(u8::try_from(len & 0x7f).unwrap() | 0x80);
            len >>= 7;
        }
        out.push(u8::try_from(len).unwrap());
    }
    out.extend_from_slice(text.as_bytes());
    out
}

/// Literal with incremental indexing, new name, raw strings.
fn literal(name: &str, value: &str) -> Vec<u8> {
    [&[0x40][..], &string(name), &string(value)].concat()
}

fn assert_malformed(block: &[u8]) {
    let mut dec = Decoder::new(4096);
    assert_eq!(
        decode(&mut dec, block),
        Err(DecodeError::Malformed),
        "{block:02x?}"
    );
}

#[test]
fn index_zero_is_an_error() {
    assert_malformed(&[0x80]);
}

#[test]
fn indices_past_the_tables_are_errors() {
    assert_malformed(&[0xbe]); // 62 with an empty dynamic table
    assert_malformed(&[0xff, 0x00]); // 127
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("a", "b")).unwrap();
    assert_eq!(decode(&mut dec, &[0xbe]).unwrap(), [pair("a", "b")]);
    assert_eq!(decode(&mut dec, &[0xbf]), Err(DecodeError::Malformed));
    // a name index past the table in a literal
    assert_malformed(&[0x7f, 0x00, 0x01, b'v']);
    assert_malformed(&[0x0f, 0x30, 0x01, b'v']);
    assert_malformed(&[0x1f, 0x30, 0x01, b'v']);
}

#[test]
fn truncated_blocks_are_errors() {
    let block = literal("custom-key", "custom-value");
    for cut in 1..block.len() {
        assert_malformed(&block[..cut]);
    }
    assert_malformed(&[0x40]); // no name
    assert_malformed(&[0x41]); // no value
    assert_malformed(&[0xff]); // index continuation missing
    assert_malformed(&[0xff, 0x80]); // continuation that never ends
}

#[test]
fn string_lengths_past_the_block_are_errors() {
    assert_malformed(&[0x00, 0x05, b'a', b'b']);
    assert_malformed(&[0x00, 0x01, b'a', 0x7f, 0xff, 0xff, 0xff, 0x0f]);
    assert_malformed(&[0x00, 0x01, b'a', 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
}

#[test]
fn integers_that_overflow_are_errors() {
    assert_malformed(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
    assert_malformed(&[0xff, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
    assert_malformed(&[0xff, 0xff, 0xff, 0xff, 0xff, 0x0f]); // 2^32 + 126
    assert_malformed(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0x0f]);
}

#[test]
fn huffman_strings_are_validated() {
    // 'a' (00011) then padding 111: valid
    let mut dec = Decoder::new(4096);
    assert_eq!(
        decode(&mut dec, &[0x00, 0x81, 0x1f, 0x00]).unwrap(),
        [pair("a", "")]
    );
    assert_malformed(&[0x00, 0x81, 0x18, 0x00]); // padding is not ones
    assert_malformed(&[0x00, 0x82, 0x1f, 0xff, 0x00]); // a whole byte of padding
    assert_malformed(&[0x00, 0x84, 0xff, 0xff, 0xff, 0xff, 0x00]); // EOS inside the data
    assert_malformed(&[0x00, 0x01, b'a', 0x81, 0x00]); // truncated: no complete symbol, not all ones
    assert_malformed(&[0x00, 0x01, b'a', 0x81, 0xfe]);
    assert_malformed(&[0x40, 0x81, 0xff, 0x01, b'v']); // padding only, as a name
}

#[test]
fn huffman_errors_are_found_even_when_the_string_is_not_kept() {
    let mut dec = Decoder::new(4096);
    let mut block = literal("a", "b");
    block.extend_from_slice(&[0x00, 0x81, 0x18, 0x00]); // after the list limit, not indexed
    assert_eq!(
        decode_with(&mut dec, &block, 10),
        Err(DecodeError::Malformed)
    );
}

#[test]
fn size_updates_are_only_allowed_at_the_start() {
    let mut dec = Decoder::new(4096);
    assert_eq!(
        decode(&mut dec, &[0x20, 0x82]).unwrap(),
        [pair(":method", "GET")]
    );
    assert_eq!(
        decode(&mut dec, &[0x20, 0x3f, 0xe1, 0x1f, 0x82])
            .unwrap()
            .len(),
        1
    );
    assert_malformed(&[0x82, 0x20]);
    assert_malformed(&[0x82, 0x3f, 0xe1, 0x1f]);
    assert_malformed(&[0x40, 0x01, b'a', 0x01, b'b', 0x20]);
}

#[test]
fn at_most_two_size_updates_open_a_block() {
    let mut dec = Decoder::new(4096);
    assert!(decode(&mut dec, &[0x20, 0x21, 0x82]).is_ok());
    assert!(decode(&mut dec, &[0x20, 0x21, 0x22, 0x82]).is_err());
    assert!(decode(&mut Decoder::new(4096), &[0x20, 0x20, 0x20]).is_err());
}

#[test]
fn size_updates_above_the_advertised_maximum_are_errors() {
    let mut dec = Decoder::new(100);
    assert!(decode(&mut dec, &[0x3f, 0x45, 0x82]).is_ok()); // 100
    assert!(decode(&mut dec, &[0x3f, 0x46, 0x82]).is_err()); // 101
    assert!(decode(&mut Decoder::new(4096), &[0x3f, 0xe2, 0x1f]).is_err()); // 4097
    assert!(decode(&mut Decoder::new(4096), &[0x3f, 0xe1, 0x1f]).is_ok()); // 4096
}

#[test]
fn size_update_to_zero_empties_the_table_and_a_later_update_restores_room() {
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("a", "b")).unwrap();
    assert_eq!(dec.table_size(), 34);
    decode(&mut dec, &[0x20]).unwrap();
    assert_eq!(dec.table_size(), 0);
    assert_eq!(decode(&mut dec, &[0xbe]), Err(DecodeError::Malformed));
    let mut block = vec![0x3f, 0xe1, 0x1f];
    block.extend(literal("c", "d"));
    decode(&mut dec, &block).unwrap();
    assert_eq!(decode(&mut dec, &[0xbe]).unwrap(), [pair("c", "d")]);
}

#[test]
fn lowering_the_advertised_maximum_requires_an_update_in_the_next_block() {
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("a", "b")).unwrap();
    dec.set_max_table_size(64);
    assert_eq!(decode(&mut dec, &[0x82]), Err(DecodeError::Malformed));
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("a", "b")).unwrap();
    dec.set_max_table_size(64);
    assert!(decode(&mut dec, &[0x3f, 0x21, 0x82]).is_ok()); // update to 64
    assert!(decode(&mut dec, &[0x82]).is_ok());
    assert!(decode(&mut dec, &[0x3f, 0x22, 0x82]).is_err()); // 65
}

#[test]
fn entries_are_evicted_oldest_first_to_make_room() {
    let mut dec = Decoder::new(100);
    decode(&mut dec, &literal("a", "1")).unwrap(); // 34
    decode(&mut dec, &literal("b", "2")).unwrap(); // 68
    decode(&mut dec, &literal("c", "3")).unwrap(); // 102 > 100: "a" goes
    assert_eq!(dec.table_size(), 68);
    assert_eq!(
        decode(&mut dec, &[0xbe, 0xbf]).unwrap(),
        [pair("c", "3"), pair("b", "2")]
    );
    assert!(decode(&mut dec, &[0xc0]).is_err());
}

#[test]
fn an_entry_larger_than_the_table_empties_it() {
    let mut dec = Decoder::new(100);
    decode(&mut dec, &literal("a", "1")).unwrap();
    let big = "x".repeat(80);
    assert_eq!(
        decode(&mut dec, &literal("big", &big)).unwrap(),
        [pair("big", &big)]
    );
    assert_eq!(dec.table_size(), 0);
    assert!(decode(&mut dec, &[0xbe]).is_err());
    decode(&mut dec, &literal("a", "1")).unwrap();
    assert_eq!(dec.table_size(), 34);
}

#[test]
fn a_dynamic_name_survives_the_eviction_its_own_insert_causes() {
    // table holds exactly one entry; the literal refers to it by name and evicts it while inserting
    let mut dec = Decoder::new(40);
    decode(&mut dec, &literal("n", "v")).unwrap();
    assert_eq!(dec.table_size(), 34);
    let got = decode(&mut dec, &[0x7e, 0x01, b'w']).unwrap();
    assert_eq!(got, [pair("n", "w")]);
    assert_eq!(decode(&mut dec, &[0xbe]).unwrap(), [pair("n", "w")]);
}

#[test]
fn literal_without_indexing_borrows_a_dynamic_name() {
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("n", "v")).unwrap();
    for first in [0x0f, 0x1f] {
        let got = decode(&mut dec, &[first, 0x2f, 0x01, b'w']).unwrap(); // index 15 + 47 = 62
        assert_eq!(got, [pair("n", "w")]);
    }
    assert_eq!(dec.table_size(), 34);
}

#[test]
fn values_may_be_empty_and_names_may_repeat() {
    let mut dec = Decoder::new(4096);
    let mut block = literal("a", "");
    block.extend(literal("a", ""));
    assert_eq!(
        decode(&mut dec, &block).unwrap(),
        [pair("a", ""), pair("a", "")]
    );
    assert_eq!(dec.table_size(), 66);
}

#[test]
fn list_limit_stops_visits_but_the_table_stays_in_step() {
    let mut dec = Decoder::new(4096);
    let mut block = literal("a", "1"); // 34
    block.extend(literal("b", "2")); // 68
    block.extend(literal("c", "3")); // 102
    block.extend(literal("d", "4"));
    let (decoded, got) = decode_with(&mut dec, &block, 70).unwrap();
    assert!(decoded.list_too_large);
    assert_eq!(got, [pair("a", "1"), pair("b", "2")]);
    assert_eq!(dec.table_size(), 4 * 34);
    let (decoded, got) = decode_with(&mut dec, &[0xbe, 0xbf, 0xc0, 0xc1], usize::MAX).unwrap();
    assert!(!decoded.list_too_large);
    assert_eq!(
        got,
        [
            pair("d", "4"),
            pair("c", "3"),
            pair("b", "2"),
            pair("a", "1")
        ]
    );
}

#[test]
fn list_limit_is_exact() {
    let mut dec = Decoder::new(4096);
    let block = literal("ab", "cd");
    assert!(!decode_with(&mut dec, &block, 36).unwrap().0.list_too_large);
    assert!(decode_with(&mut dec, &block, 35).unwrap().0.list_too_large);
    assert!(!decode_with(&mut dec, &[0x82], 42).unwrap().0.list_too_large);
    assert!(decode_with(&mut dec, &[0x82], 41).unwrap().0.list_too_large);
}

#[test]
fn indexed_references_to_a_large_entry_do_not_amplify() {
    let value = "v".repeat(4000);
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("big", &value)).unwrap();
    let block = vec![0xbe; 200_000];
    let (decoded, got) = decode_with(&mut dec, &block, 10_000).unwrap();
    assert!(decoded.list_too_large);
    assert_eq!(got.len(), 2); // 2 x 4035 fits, the third crosses 10 000
    assert_eq!(dec.table_size(), 4035);
}

fn encoded(name: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(4096).encode_field(
        &mut out,
        name.as_bytes(),
        value.as_bytes(),
        Indexing::Incremental,
    );
    out
}

#[test]
fn huffman_strings_too_large_to_keep_are_validated_and_empty_the_table() {
    // 6000 octets decode from 3750: more than both the list limit and the table
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &literal("a", "b")).unwrap();
    let (decoded, got) = decode_with(&mut dec, &encoded("k", &"a".repeat(6000)), 100).unwrap();
    assert!(decoded.list_too_large && got.is_empty());
    assert_eq!(dec.table_size(), 0);
    assert_eq!(decode(&mut dec, &[0x82]).unwrap(), [pair(":method", "GET")]);
}

#[test]
fn huffman_strings_that_fit_the_table_are_kept_for_it_even_past_the_list_limit() {
    let mut dec = Decoder::new(4096);
    let value = "a".repeat(3000);
    let (decoded, got) = decode_with(&mut dec, &encoded("k", &value), 100).unwrap();
    assert!(decoded.list_too_large && got.is_empty());
    assert_eq!(dec.table_size(), 3033);
    assert_eq!(decode(&mut dec, &[0xbe]).unwrap(), [pair("k", &value)]);
}

#[test]
fn long_literals_with_multi_octet_lengths_decode() {
    let mut dec = Decoder::new(100_000);
    let value = "x".repeat(20_000);
    let block = [&[0x00][..], &string("k"), &string(&value)].concat();
    assert_eq!(decode(&mut dec, &block).unwrap(), [pair("k", &value)]);
}

#[test]
fn arbitrary_blocks_never_panic() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut dec = Decoder::new(512);
    for _ in 0..20_000 {
        let len = usize::try_from(next() % 64).unwrap();
        let block: Vec<u8> = (0..len).map(|_| next().to_le_bytes()[0]).collect();
        let _ = decode_with(&mut dec, &block, 4096);
    }
}
