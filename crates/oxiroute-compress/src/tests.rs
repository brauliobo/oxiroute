use std::io::Read;

use crate::{AcceptEncoding, Coding, Levels, compress};

const SERVER_ORDER: [Coding; 3] = [Coding::Zstd, Coding::Brotli, Coding::Gzip];

fn select(header: &str) -> Option<Coding> {
    AcceptEncoding::parse([header.as_bytes()]).select(&SERVER_ORDER)
}

#[test]
fn negotiation_matrix() {
    let cases: &[(&str, Option<Coding>)] = &[
        ("", None),
        ("identity", None),
        ("gzip", Some(Coding::Gzip)),
        ("GZIP", Some(Coding::Gzip)),
        ("x-gzip", Some(Coding::Gzip)),
        ("br", Some(Coding::Brotli)),
        ("zstd", Some(Coding::Zstd)),
        ("deflate", None),
        ("gzip, deflate, br, zstd", Some(Coding::Zstd)),
        ("gzip, deflate, br", Some(Coding::Brotli)),
        ("gzip;q=1.0, br;q=0.5", Some(Coding::Gzip)),
        ("gzip;q=0.8, br;q=0.9, zstd;q=0.1", Some(Coding::Brotli)),
        ("gzip;q=0", None),
        ("gzip;q=0, br", Some(Coding::Brotli)),
        ("*", Some(Coding::Zstd)),
        ("*;q=0.5, gzip", Some(Coding::Gzip)),
        ("gzip;q=0.5, *;q=0.9", Some(Coding::Zstd)),
        ("*, zstd;q=0, br;q=0", Some(Coding::Gzip)),
        ("*;q=0", None),
        ("*;q=0, gzip", Some(Coding::Gzip)),
        ("identity;q=1, gzip;q=0.5", None),
        ("identity;q=0.5, gzip;q=0.5", Some(Coding::Gzip)),
        ("identity;q=0.4, gzip;q=0.5", Some(Coding::Gzip)),
        ("identity;q=0, gzip;q=0", None),
        ("gzip;q=0.001", Some(Coding::Gzip)),
        ("gzip ; q = 0.7", Some(Coding::Gzip)),
        ("gzip;Q=0.7", Some(Coding::Gzip)),
        ("gzip;q=1.5", None),
        ("gzip;q=0.1234", None),
        ("gzip;q=abc, br", Some(Coding::Brotli)),
        (",,gzip,,", Some(Coding::Gzip)),
        ("gzip;q=0, gzip", None),
        ("gzip;level=5", Some(Coding::Gzip)),
    ];
    for (header, expected) in cases {
        assert_eq!(select(header), *expected, "Accept-Encoding: {header:?}");
    }
}

#[test]
fn identity_preference_beats_lower_weighted_codings() {
    assert_eq!(select("identity;q=0.9, gzip;q=0.8"), None);
    assert_eq!(select("identity;q=0.8, gzip;q=0.9"), Some(Coding::Gzip));
}

#[test]
fn server_order_breaks_equal_weights() {
    let accepted = AcceptEncoding::parse([b"gzip, br, zstd".as_slice()]);
    assert_eq!(
        accepted.select(&[Coding::Gzip, Coding::Brotli, Coding::Zstd]),
        Some(Coding::Gzip)
    );
    assert_eq!(
        accepted.select(&[Coding::Brotli, Coding::Zstd, Coding::Gzip]),
        Some(Coding::Brotli)
    );
    assert_eq!(accepted.select(&[]), None);
}

#[test]
fn repeated_header_lines_combine() {
    let accepted = AcceptEncoding::parse([b"gzip;q=0.2".as_slice(), b"br;q=0.7".as_slice()]);
    assert_eq!(accepted.select(&SERVER_ORDER), Some(Coding::Brotli));
}

#[test]
fn headers_without_accept_encoding_select_nothing() {
    let headers = http::HeaderMap::new();
    assert_eq!(
        AcceptEncoding::from_headers(&headers).select(&SERVER_ORDER),
        None
    );
}

fn body() -> Vec<u8> {
    (0..4000)
        .flat_map(|n| format!("<li class=\"row\">item {n}</li>\n").into_bytes())
        .collect()
}

fn decode(coding: Coding, encoded: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    match coding {
        Coding::Gzip => flate2::read::GzDecoder::new(encoded)
            .read_to_end(&mut out)
            .map(drop),
        Coding::Brotli => brotli::Decompressor::new(encoded, 4096)
            .read_to_end(&mut out)
            .map(drop),
        Coding::Zstd => zstd::decode_all(encoded).map(|decoded| out = decoded),
    }
    .expect("the encoded body decodes");
    out
}

#[test]
fn every_coding_round_trips_at_default_and_extreme_levels() {
    let body = body();
    let extremes = [
        Levels {
            gzip: 1,
            brotli: 0,
            zstd: -5,
        },
        Levels::default(),
        Levels {
            gzip: 12,
            brotli: 11,
            zstd: 19,
        },
    ];
    for levels in extremes {
        for coding in Coding::ALL {
            let encoded = compress(coding, levels, &body).unwrap();
            assert!(encoded.len() < body.len(), "{coding:?} {levels:?}");
            assert_eq!(decode(coding, &encoded), body, "{coding:?} {levels:?}");
        }
    }
}

#[test]
fn empty_body_round_trips() {
    for coding in Coding::ALL {
        let encoded = compress(coding, Levels::default(), b"").unwrap();
        assert!(decode(coding, &encoded).is_empty(), "{coding:?}");
    }
}

#[test]
fn out_of_range_levels_are_rejected() {
    assert!(
        compress(
            Coding::Gzip,
            Levels {
                gzip: 13,
                ..Levels::default()
            },
            b"x"
        )
        .is_err()
    );
    assert!(
        compress(
            Coding::Brotli,
            Levels {
                brotli: 12,
                ..Levels::default()
            },
            b"x"
        )
        .is_err()
    );
    assert!(
        compress(
            Coding::Zstd,
            Levels {
                zstd: 100,
                ..Levels::default()
            },
            b"x"
        )
        .is_err()
    );
}
