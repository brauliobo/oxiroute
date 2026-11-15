//! Shared scanner contracts against independent byte-wise predicates at every vector boundary.

use oxiroute_cpu::{
    Level, ascii_range, ascii_range_with, find_any3, find_any3_with, header_value_end,
    header_value_end_with, json_escape_at, json_escape_at_with,
};

const EDGES: [usize; 16] = [
    0, 1, 7, 15, 16, 17, 31, 32, 33, 63, 64, 65, 95, 96, 127, 128,
];
const NEEDLES: [u8; 3] = *b"/%\\";

fn check(level: Level, buf: &[u8], from: usize) {
    let rest = buf.get(from..).unwrap_or_default();
    let any = rest.iter().position(|b| NEEDLES.contains(b));
    let header = rest
        .iter()
        .position(|&b| !((b >= 32 && b != 127) || b == b'\t'));
    let json = buf.iter().position(|&b| b < 32 || b == b'"' || b == b'\\');
    assert_eq!(
        find_any3_with(level, buf, from, NEEDLES),
        from + any.unwrap_or(rest.len())
    );
    assert_eq!(
        find_any3(buf, from, NEEDLES),
        from + any.unwrap_or(rest.len())
    );
    assert_eq!(
        header_value_end_with(level, buf, from),
        from + header.unwrap_or(rest.len())
    );
    assert_eq!(
        header_value_end(buf, from),
        from + header.unwrap_or(rest.len())
    );
    assert_eq!(json_escape_at_with(level, buf), json.unwrap_or(buf.len()));
    assert_eq!(json_escape_at(buf), json.unwrap_or(buf.len()));
    for (low, high) in [
        (32, 126),
        (33, 126),
        (0, 255),
        (128, 255),
        (127, 127),
        (255, 0),
    ] {
        assert_eq!(
            ascii_range_with(level, buf, low, high),
            buf.iter().all(|&b| (low..=high).contains(&b)),
            "{level:?} len={} range={low}..={high}",
            buf.len()
        );
        assert_eq!(
            ascii_range(buf, low, high),
            buf.iter().all(|&b| (low..=high).contains(&b))
        );
    }
}

#[test]
fn every_byte_at_vector_edges_matches_independent_predicates() {
    for level in Level::supported() {
        for len in [
            0, 1, 7, 15, 16, 17, 31, 32, 33, 63, 64, 65, 95, 96, 127, 128, 129, 130,
        ] {
            let mut buf = vec![b'a'; len];
            check(level, &buf, 0);
            for pos in EDGES.into_iter().filter(|&pos| pos < len) {
                for b in 0..=255 {
                    buf[pos] = b;
                    check(level, &buf, 0);
                    check(level, &buf, pos);
                    check(level, &buf, pos + 1);
                }
                buf[pos] = b'a';
            }
        }
    }
}

#[test]
fn unaligned_inputs_and_multiple_hits_return_the_first_match() {
    let mut storage = vec![b'x'; 256];
    for level in Level::supported() {
        for shift in 0..64 {
            let buf = &mut storage[shift..shift + 130];
            for first in EDGES {
                for second in [first + 1, (first + 64).min(129)] {
                    buf[first] = b'\\';
                    buf[second] = 0;
                    for from in [
                        0,
                        first,
                        first + 1,
                        second,
                        second + 1,
                        buf.len(),
                        usize::MAX,
                    ] {
                        check(level, buf, from);
                    }
                    buf[first] = b'x';
                    buf[second] = b'x';
                }
            }
        }
    }
}

#[test]
fn arbitrary_and_repeated_needles_and_unsupported_levels_are_exact() {
    let buf: Vec<u8> = (0..=255).collect();
    for level in Level::ALL {
        for needles in [[0, 127, 255], [255; 3], [2, 2, 1], *b"aAa"] {
            for from in 0..=buf.len() {
                let expected = buf[from..].iter().position(|b| needles.contains(b));
                assert_eq!(
                    find_any3_with(level, &buf, from, needles),
                    from + expected.unwrap_or(buf.len() - from)
                );
            }
        }
        assert_eq!(find_any3_with(level, &[], usize::MAX, NEEDLES), usize::MAX);
        assert_eq!(header_value_end_with(level, &[], usize::MAX), usize::MAX);
        assert_eq!(json_escape_at_with(level, b"\x7f\x80\xff"), 3);
        assert_eq!(header_value_end_with(level, b"\t\x80\xff\r", 0), 3);
    }
}
