//! RFC 7541 Appendix C: every request and response example, decoded and re-encoded, with the dynamic table
//! checked after each step.
use oxiroute_h2::hpack::{Decoder, Encoder, Indexing};

type Fields = Vec<(Vec<u8>, Vec<u8>)>;

fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn fields(list: &[(&str, &str)]) -> Fields {
    list.iter()
        .map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect()
}

fn decode(dec: &mut Decoder, block: &[u8]) -> Fields {
    let mut out = Vec::new();
    let decoded = dec
        .decode(block, usize::MAX, |n, v| out.push((n.to_vec(), v.to_vec())))
        .unwrap();
    assert!(!decoded.list_too_large);
    out
}

/// Dynamic table content, newest first, read back through indexed representations.
fn table(decoder: &mut Decoder, entries: usize) -> Fields {
    let mut out = Vec::new();
    for n in 0..entries {
        out.extend(decode(decoder, &[0x80 | u8::try_from(62 + n).unwrap()]));
    }
    assert!(
        decoder
            .decode(
                &[0x80 | u8::try_from(62 + entries).unwrap()],
                usize::MAX,
                |_, _| ()
            )
            .is_err()
    );
    out
}

struct Step {
    wire: &'static str,
    fields: &'static [(&'static str, &'static str)],
    table: &'static [(&'static str, &'static str)],
    size: usize,
}

fn run_decoder(table_size: usize, steps: &[Step]) {
    let mut decoder = Decoder::new(table_size);
    for (i, step) in steps.iter().enumerate() {
        assert_eq!(
            decode(&mut decoder, &hex(step.wire)),
            fields(step.fields),
            "step {i} fields"
        );
        assert_eq!(decoder.table_size(), step.size, "step {i} table size");
        assert_eq!(
            table(&mut decoder, step.table.len()),
            fields(step.table),
            "step {i} table"
        );
    }
}

/// The encoder must produce the exact octets of the Huffman examples, then track the same table. The RFC codes a
/// string with Huffman even when that is not shorter; this encoder keeps such strings raw, so those steps (`loose`)
/// are checked by decoding instead.
fn run_encoder_exact(table_size: usize, steps: &[Step], loose: &[usize]) {
    let (mut encoder, mut decoder) = (Encoder::new(table_size), Decoder::new(table_size));
    for (i, step) in steps.iter().enumerate() {
        let mut out = Vec::new();
        for (n, v) in step.fields {
            encoder.encode_field(&mut out, n.as_bytes(), v.as_bytes(), Indexing::Incremental);
        }
        if loose.contains(&i) {
            assert_eq!(
                decode(&mut decoder, &out),
                fields(step.fields),
                "step {i} fields"
            );
        } else {
            assert_eq!(out, hex(step.wire), "step {i} octets");
            decode(&mut decoder, &out);
        }
        assert_eq!(encoder.table_size(), step.size, "step {i} table size");
    }
}

const REQUESTS_RAW: [Step; 3] = [
    Step {
        wire: "8286 8441 0f77 7777 2e65 7861 6d70 6c65 2e63 6f6d",
        fields: &[
            (":method", "GET"),
            (":scheme", "http"),
            (":path", "/"),
            (":authority", "www.example.com"),
        ],
        table: &[(":authority", "www.example.com")],
        size: 57,
    },
    Step {
        wire: "8286 84be 5808 6e6f 2d63 6163 6865",
        fields: &[
            (":method", "GET"),
            (":scheme", "http"),
            (":path", "/"),
            (":authority", "www.example.com"),
            ("cache-control", "no-cache"),
        ],
        table: &[
            ("cache-control", "no-cache"),
            (":authority", "www.example.com"),
        ],
        size: 110,
    },
    Step {
        wire: "8287 85bf 400a 6375 7374 6f6d 2d6b 6579 0c63 7573 746f 6d2d 7661 6c75 65",
        fields: &[
            (":method", "GET"),
            (":scheme", "https"),
            (":path", "/index.html"),
            (":authority", "www.example.com"),
            ("custom-key", "custom-value"),
        ],
        table: &[
            ("custom-key", "custom-value"),
            ("cache-control", "no-cache"),
            (":authority", "www.example.com"),
        ],
        size: 164,
    },
];

const REQUESTS_HUFFMAN: [Step; 3] = [
    Step {
        wire: "8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff",
        ..REQUESTS_RAW[0]
    },
    Step {
        wire: "8286 84be 5886 a8eb 1064 9cbf",
        ..REQUESTS_RAW[1]
    },
    Step {
        wire: "8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf",
        ..REQUESTS_RAW[2]
    },
];

const LOCATION: (&str, &str) = ("location", "https://www.example.com");

const RESPONSES_RAW: [Step; 3] = [
    Step {
        wire: "4803 3330 3258 0770 7269 7661 7465 611d 4d6f 6e2c 2032 3120 4f63 7420 3230 3133 2032 303a 3133 3a32
               3120 474d 546e 1768 7474 7073 3a2f 2f77 7777 2e65 7861 6d70 6c65 2e63 6f6d",
        fields: &[
            (":status", "302"),
            ("cache-control", "private"),
            ("date", "Mon, 21 Oct 2013 20:13:21 GMT"),
            LOCATION,
        ],
        table: &[
            LOCATION,
            ("date", "Mon, 21 Oct 2013 20:13:21 GMT"),
            ("cache-control", "private"),
            (":status", "302"),
        ],
        size: 222,
    },
    Step {
        wire: "4803 3330 37c1 c0bf",
        fields: &[
            (":status", "307"),
            ("cache-control", "private"),
            ("date", "Mon, 21 Oct 2013 20:13:21 GMT"),
            LOCATION,
        ],
        table: &[
            (":status", "307"),
            LOCATION,
            ("date", "Mon, 21 Oct 2013 20:13:21 GMT"),
            ("cache-control", "private"),
        ],
        size: 222,
    },
    Step {
        wire: "88c1 611d 4d6f 6e2c 2032 3120 4f63 7420 3230 3133 2032 303a 3133 3a32 3220 474d 54c0 5a04 677a 6970 7738
               666f 6f3d 4153 444a 4b48 514b 425a 584f 5157 454f 5049 5541 5851 5745 4f49 553b 206d 6178 2d61 6765 3d33
               3630 303b 2076 6572 7369 6f6e 3d31",
        fields: &[
            (":status", "200"),
            ("cache-control", "private"),
            ("date", "Mon, 21 Oct 2013 20:13:22 GMT"),
            LOCATION,
            ("content-encoding", "gzip"),
            ("set-cookie", "foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1"),
        ],
        table: &[
            ("set-cookie", "foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1"),
            ("content-encoding", "gzip"),
            ("date", "Mon, 21 Oct 2013 20:13:22 GMT"),
        ],
        size: 215,
    },
];

const RESPONSES_HUFFMAN: [Step; 3] = [
    Step {
        wire: "4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 9504 0b81 66e0 82a6 2d1b ff6e 919d 29ad
               1718 63c7 8f0b 97c8 e9ae 82ae 43d3",
        ..RESPONSES_RAW[0]
    },
    Step { wire: "4883 640e ffc1 c0bf", ..RESPONSES_RAW[1] },
    Step {
        wire: "88c1 6196 d07a be94 1054 d444 a820 0595 040b 8166 e084 a62d 1bff c05a 839b d9ab 77ad 94e7 821d d7f2
               e6c7 b335 dfdf cd5b 3960 d5af 2708 7f36 72c1 ab27 0fb5 291f 9587 3160 65c0 03ed 4ee5 b106 3d50 07",
        ..RESPONSES_RAW[2]
    },
];

#[test]
fn c3_requests_without_huffman() {
    run_decoder(4096, &REQUESTS_RAW);
}

#[test]
fn c4_requests_with_huffman_decode_and_encode_exactly() {
    run_decoder(4096, &REQUESTS_HUFFMAN);
    run_encoder_exact(4096, &REQUESTS_HUFFMAN, &[]);
}

#[test]
fn c5_responses_without_huffman_evict_in_a_256_octet_table() {
    run_decoder(256, &RESPONSES_RAW);
}

#[test]
fn c6_responses_with_huffman_decode_and_encode_exactly() {
    run_decoder(256, &RESPONSES_HUFFMAN);
    run_encoder_exact(256, &RESPONSES_HUFFMAN, &[1]);
}

#[test]
fn raw_requests_are_decoded_by_a_decoder_fed_the_huffman_ones_too() {
    // the same dynamic table results whichever string form the peer chose
    let mut decoder = Decoder::new(4096);
    for (raw, huffman) in REQUESTS_RAW.iter().zip(&REQUESTS_HUFFMAN) {
        assert_eq!(decode(&mut decoder, &hex(huffman.wire)), fields(raw.fields));
    }
}

#[test]
fn c2_single_representations() {
    let mut decoder = Decoder::new(4096);
    let wire = hex("400a 6375 7374 6f6d 2d6b 6579 0d63 7573 746f 6d2d 6865 6164 6572");
    assert_eq!(
        decode(&mut decoder, &wire),
        fields(&[("custom-key", "custom-header")])
    );
    assert_eq!(decoder.table_size(), 55);
    assert_eq!(
        table(&mut decoder, 1),
        fields(&[("custom-key", "custom-header")])
    );

    let mut decoder = Decoder::new(4096);
    let wire = hex("040c 2f73 616d 706c 652f 7061 7468");
    assert_eq!(
        decode(&mut decoder, &wire),
        fields(&[(":path", "/sample/path")])
    );
    assert_eq!(decoder.table_size(), 0);

    let wire = hex("1008 7061 7373 776f 7264 0673 6563 7265 74");
    assert_eq!(
        decode(&mut decoder, &wire),
        fields(&[("password", "secret")])
    );
    assert_eq!(decoder.table_size(), 0);

    assert_eq!(
        decode(&mut decoder, &hex("82")),
        fields(&[(":method", "GET")])
    );
    assert_eq!(decoder.table_size(), 0);
}

#[test]
fn c2_encoder_representations_decode_to_the_same_fields() {
    let cases = [
        (
            "custom-key",
            "custom-header",
            Indexing::Incremental,
            0x40,
            55,
        ),
        (":path", "/sample/path", Indexing::None, 0x04, 0),
        ("password", "secret", Indexing::Never, 0x10, 0),
    ];
    for (name, value, indexing, first, size) in cases {
        let mut encoder = Encoder::new(4096);
        let mut out = Vec::new();
        encoder.encode_field(&mut out, name.as_bytes(), value.as_bytes(), indexing);
        assert_eq!(out[0], first, "{name}");
        assert_eq!(encoder.table_size(), size);
        assert_eq!(
            decode(&mut Decoder::new(4096), &out),
            fields(&[(name, value)])
        );
    }
    let mut out = Vec::new();
    Encoder::new(4096).encode_field(&mut out, b":method", b"GET", Indexing::Incremental);
    assert_eq!(out, [0x82]);
}
