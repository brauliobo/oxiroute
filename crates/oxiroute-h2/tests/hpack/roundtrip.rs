//! Encoder output fed to the decoder: random field lists, random table sizes and indexing modes, size changes.
use oxiroute_h2::hpack::{Decoder, Encoder, Indexing};

type Fields = Vec<(Vec<u8>, Vec<u8>)>;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const NAMES: [&str; 24] = [
    ":authority",
    ":method",
    ":path",
    ":scheme",
    ":status",
    "accept",
    "accept-encoding",
    "cache-control",
    "content-length",
    "content-type",
    "cookie",
    "date",
    "etag",
    "location",
    "server",
    "set-cookie",
    "user-agent",
    "vary",
    "x-request-id",
    "x-forwarded-for",
    "x-a",
    "x-b",
    "x-long-custom-header-name-for-the-test",
    "www-authenticate",
];

const VALUES: [&str; 12] = [
    "",
    "GET",
    "200",
    "gzip, deflate",
    "https",
    "/",
    "text/html; charset=utf-8",
    "no-cache",
    "example.com",
    "gzip",
    "max-age=3600",
    "Mon, 21 Oct 2013 20:13:21 GMT",
];

fn random_string(rng: &mut Rng) -> Vec<u8> {
    let len = match rng.below(20) {
        0 => 100 + rng.below(3000),
        1..=4 => rng.below(150),
        _ => rng.below(20),
    };
    let wide = rng.below(8) == 0;
    (0..len)
        .map(|_| {
            if wide {
                rng.next().to_le_bytes()[0]
            } else {
                32 + u8::try_from(rng.below(95)).unwrap()
            }
        })
        .collect()
}

fn random_field(rng: &mut Rng, seen: &Fields) -> (Vec<u8>, Vec<u8>) {
    if !seen.is_empty() && rng.below(3) == 0 {
        return rng.pick(seen).clone();
    }
    let name = match rng.below(6) {
        0 => format!("x-{}", rng.below(40)).into_bytes(),
        _ => rng.pick(&NAMES).as_bytes().to_vec(),
    };
    let value = match rng.below(3) {
        0 => random_string(rng),
        _ => rng.pick(&VALUES).as_bytes().to_vec(),
    };
    (name, value)
}

fn random_indexing(rng: &mut Rng) -> Indexing {
    *rng.pick(&[
        Indexing::Incremental,
        Indexing::Incremental,
        Indexing::None,
        Indexing::Never,
    ])
}

fn decode_all(dec: &mut Decoder, block: &[u8]) -> Fields {
    let mut out = Vec::new();
    let decoded = dec
        .decode(block, usize::MAX, |n, v| out.push((n.to_vec(), v.to_vec())))
        .unwrap();
    assert!(!decoded.list_too_large);
    out
}

#[test]
fn random_blocks_round_trip_for_every_table_size() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for table_size in [0, 1, 32, 33, 64, 100, 256, 1000, 4096, 65_536] {
        let (mut enc, mut dec) = (Encoder::new(table_size), Decoder::new(table_size));
        let mut seen = Fields::new();
        for block_no in 0..300 {
            let (mut expected, mut block) = (Fields::new(), Vec::new());
            for _ in 0..rng.below(12) {
                let (name, value) = random_field(&mut rng, &seen);
                enc.encode_field(&mut block, &name, &value, random_indexing(&mut rng));
                expected.push((name, value));
            }
            assert_eq!(
                decode_all(&mut dec, &block),
                expected,
                "table {table_size} block {block_no}"
            );
            assert_eq!(
                enc.table_size(),
                dec.table_size(),
                "table {table_size} block {block_no}"
            );
            seen.extend(expected);
            seen.truncate(40);
        }
    }
}

#[test]
fn random_table_size_changes_stay_in_step() {
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    let limit = 65_536;
    let (mut enc, mut dec) = (Encoder::new(limit), Decoder::new(limit));
    let mut seen = Fields::new();
    for block_no in 0..2000 {
        for _ in 0..rng.below(3) {
            let size = *rng.pick(&[0, 1, 32, 64, 100, 256, 1000, 4096, 10_000, 65_536]);
            dec.set_max_table_size(size);
            enc.set_max_table_size(size);
        }
        let (mut expected, mut block) = (Fields::new(), Vec::new());
        for _ in 0..=rng.below(8) {
            let (name, value) = random_field(&mut rng, &seen);
            enc.encode_field(&mut block, &name, &value, random_indexing(&mut rng));
            expected.push((name, value));
        }
        assert_eq!(decode_all(&mut dec, &block), expected, "block {block_no}");
        assert_eq!(enc.table_size(), dec.table_size(), "block {block_no}");
        seen.extend(expected);
        seen.truncate(40);
    }
}

fn first_bytes(enc: &mut Encoder) -> Vec<u8> {
    let mut out = Vec::new();
    enc.encode_field(&mut out, b":method", b"GET", Indexing::Incremental);
    out
}

#[test]
fn a_size_change_is_announced_at_the_start_of_the_next_block_only() {
    let mut enc = Encoder::new(4096);
    assert_eq!(first_bytes(&mut enc), [0x82]);
    enc.set_max_table_size(0);
    assert_eq!(first_bytes(&mut enc), [0x20, 0x82]);
    assert_eq!(first_bytes(&mut enc), [0x82]);
    enc.set_max_table_size(100);
    assert_eq!(first_bytes(&mut enc), [0x3f, 0x45, 0x82]);
}

#[test]
fn the_smallest_size_is_announced_before_the_final_one() {
    let mut enc = Encoder::new(4096);
    enc.set_max_table_size(100);
    enc.set_max_table_size(4096);
    assert_eq!(first_bytes(&mut enc), [0x3f, 0x45, 0x3f, 0xe1, 0x1f, 0x82]);
    assert_eq!(first_bytes(&mut enc), [0x82]);

    let mut enc = Encoder::new(4096);
    enc.set_max_table_size(50);
    enc.set_max_table_size(100);
    assert_eq!(first_bytes(&mut enc), [0x3f, 0x13, 0x3f, 0x45, 0x82]);

    let mut enc = Encoder::new(4096);
    enc.set_max_table_size(100);
    enc.set_max_table_size(50);
    assert_eq!(first_bytes(&mut enc), [0x3f, 0x13, 0x82]);
}

#[test]
fn setting_the_current_size_announces_nothing() {
    let mut enc = Encoder::new(4096);
    enc.set_max_table_size(4096);
    assert_eq!(first_bytes(&mut enc), [0x82]);
}

#[test]
fn a_peer_table_larger_than_the_encoder_keeps_is_trimmed_in_the_first_block() {
    let mut enc = Encoder::new(100_000);
    assert_eq!(first_bytes(&mut enc), [0x3f, 0xe1, 0x1f, 0x82]);
    let mut dec = Decoder::new(100_000);
    let mut block = Vec::new();
    Encoder::new(100_000).encode_field(&mut block, b"a", b"b", Indexing::Incremental);
    assert_eq!(
        decode_all(&mut dec, &block),
        [(b"a".to_vec(), b"b".to_vec())]
    );
}

#[test]
fn the_announced_sizes_decode_and_evict_the_decoder_table() {
    let (mut enc, mut dec) = (Encoder::new(4096), Decoder::new(4096));
    let mut block = Vec::new();
    enc.encode_field(&mut block, b"a", b"b", Indexing::Incremental);
    decode_all(&mut dec, &block);
    assert_eq!(dec.table_size(), 34);
    enc.set_max_table_size(10);
    enc.set_max_table_size(4096);
    let mut block = Vec::new();
    enc.encode_field(&mut block, b"c", b"d", Indexing::Incremental);
    decode_all(&mut dec, &block);
    assert_eq!((enc.table_size(), dec.table_size()), (34, 34));
}

#[test]
fn indexing_modes_choose_the_representation() {
    let mut enc = Encoder::new(4096);
    let mut out = Vec::new();
    enc.encode_field(&mut out, b"x-a", b"1", Indexing::Incremental);
    let first = out.len();
    enc.encode_field(&mut out, b"x-a", b"1", Indexing::Incremental);
    assert_eq!(out[first..], [0xbe], "known pair is one indexed octet");
    out.clear();
    enc.encode_field(&mut out, b"x-a", b"1", Indexing::Never);
    assert_eq!(
        out[0], 0x1f,
        "never indexed stays a literal even for a known pair"
    );
    out.clear();
    enc.encode_field(&mut out, b":method", b"GET", Indexing::Never);
    assert_eq!(out[0], 0x12);
    out.clear();
    enc.encode_field(&mut out, b"x-b", b"1", Indexing::None);
    assert_eq!(out[0], 0x00, "no indexing with a new name");
    assert_eq!(enc.table_size(), 36, "only the first field was entered");
}

#[test]
fn long_strings_round_trip_with_multi_octet_lengths() {
    let (mut enc, mut dec) = (Encoder::new(4096), Decoder::new(4096));
    for len in [125, 126, 127, 128, 200, 1000, 16_384, 70_000] {
        for fill in [b'a', 0xfe] {
            let value = vec![fill; len];
            let mut block = Vec::new();
            enc.encode_field(&mut block, b"x-long", &value, Indexing::Incremental);
            assert_eq!(
                decode_all(&mut dec, &block),
                [(b"x-long".to_vec(), value)],
                "len {len}"
            );
        }
    }
}
