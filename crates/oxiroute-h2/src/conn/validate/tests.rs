//! The byte validators against the `http` types they stand in for.
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::uri::PathAndQuery;

use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn string(&mut self, alphabet: &[u8], max: usize) -> Vec<u8> {
        (0..=self.below(max))
            .map(|_| alphabet[self.below(alphabet.len())])
            .collect()
    }
}

const PATH_ALPHABET: &[u8] =
    b"/?#%&=a1~-._ \"{}<>\\^`|[]@:;,!$'()*+\x00\x01\x7f\x80\xc3\xa9\xff\xe2\x82\xac\t";
const NAME_ALPHABET: &[u8] = b"abcxyz019-_.!#$%&'*+^`|~\"AZ:/ ()<>@,;=?[]{}\\\x00\x7f\x80\xff";

#[test]
fn every_single_byte_name_and_value_agrees_with_http() {
    for b in 0..=255u8 {
        assert_eq!(
            name(&[b]),
            HeaderName::from_lowercase(&[b]).is_ok(),
            "name byte {b}"
        );
        assert_eq!(
            value(&[b]),
            HeaderValue::from_bytes(&[b]).is_ok(),
            "value byte {b}"
        );
    }
}

#[test]
fn names_agree_with_http() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..200_000 {
        let n = rng.string(NAME_ALPHABET, 80);
        assert_eq!(name(&n), HeaderName::from_lowercase(&n).is_ok(), "{n:?}");
    }
    assert!(!name(b""));
}

#[test]
fn name_length_limits_agree_with_http() {
    for len in [63, 64, 65, 1000, 65_534, 65_535, 65_536, 70_000] {
        let n = vec![b'a'; len];
        assert_eq!(name(&n), HeaderName::from_lowercase(&n).is_ok(), "{len}");
    }
}

#[test]
fn values_agree_with_http() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..100_000 {
        let v = rng.string(b"ab c\t;=\x00\x01\x1f\x7f\x80\xff\r\n\"", 40);
        assert_eq!(value(&v), HeaderValue::from_bytes(&v).is_ok(), "{v:?}");
    }
}

fn http_path(p: &[u8]) -> Option<Vec<u8>> {
    let shared = PathAndQuery::from_maybe_shared(Bytes::copy_from_slice(p)).ok();
    let borrowed = PathAndQuery::try_from(p).ok();
    assert_eq!(shared, borrowed, "{p:?}");
    shared.map(|pq| pq.as_str().as_bytes().to_vec()) // "/" for a path that was only a fragment
}

#[test]
fn paths_agree_with_http() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..400_000 {
        let mut p = rng.string(PATH_ALPHABET, 24);
        if rng.below(3) != 0 {
            p.insert(0, b"/?#*"[rng.below(4)]);
        }
        assert_eq!(path(&p).map(<[u8]>::to_vec), http_path(&p), "{p:?}");
    }
}

#[test]
fn every_single_byte_path_agrees_with_http() {
    for b in 0..=255u8 {
        for prefix in [&b"/"[..], b"/?", b"?", b"#", b"/a?b"] {
            let p = [prefix, &[b], b"x"].concat();
            assert_eq!(path(&p).map(<[u8]>::to_vec), http_path(&p), "{p:?}");
            let p = [prefix, &[b]].concat();
            assert_eq!(path(&p).map(<[u8]>::to_vec), http_path(&p), "{p:?}");
        }
    }
}

#[test]
fn path_shapes_and_lengths_agree_with_http() {
    let mut cases: Vec<Vec<u8>> = [
        &b""[..],
        b"*",
        b"**",
        b"/",
        b"?",
        b"#",
        b"/#",
        b"/a#\xff\x00",
        b"/a?b#c",
        b"/\xff",
        b"/a?\xc3\xa9#\xff",
        b"/?\xff#",
        b"a",
        b"//",
        b"/a b",
    ]
    .iter()
    .map(|p| p.to_vec())
    .collect();
    for len in [65_533, 65_534, 65_535, 65_536] {
        cases.push([&b"/"[..], &vec![b'a'; len - 1]].concat());
        cases.push([&b"/#"[..], &vec![b'\xff'; len - 2]].concat());
    }
    for p in cases {
        assert_eq!(path(&p).map(<[u8]>::to_vec), http_path(&p), "{}", p.len());
    }
}

#[test]
fn fields_apply_the_request_rules() {
    let mut cl = None;
    assert!(field(b"accept", b"*/*", &mut cl));
    assert!(!field(b"Accept", b"*/*", &mut cl));
    assert!(!field(b"connection", b"close", &mut cl));
    assert!(!field(b"transfer-encoding", b"chunked", &mut cl));
    assert!(!field(b"upgrade", b"h2c", &mut cl));
    assert!(!field(b"keep-alive", b"1", &mut cl));
    assert!(!field(b"proxy-connection", b"1", &mut cl));
    assert!(field(b"te", b"trailers", &mut cl));
    assert!(!field(b"te", b"gzip", &mut cl));
    assert_eq!(cl, None);
    assert!(field(b"content-length", b"12", &mut cl));
    assert!(field(b"content-length", b"12", &mut cl));
    assert!(!field(b"content-length", b"13", &mut cl));
    assert!(!field(b"content-length", b"+1", &mut cl));
    assert!(!field(b"content-length", b"", &mut cl));
    assert!(!field(b"content-length", &[b'1'; 20], &mut cl));
    assert_eq!(cl, Some(12));
}
