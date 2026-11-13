//! Every level the CPU runs against a byte-at-a-time oracle, on every byte value at positions that cross the 8, 16, 32
//! and 64 byte steps of the kernels; levels above the hardware are clamped and must agree too.

use oxiroute_cpu::{Level, TokenRun, control_at_with, is_tchar, token_run_with};

/// Positions around the step boundaries of every kernel, and the ends of the longest buffer.
const EDGES: [usize; 24] = [
    0, 1, 2, 6, 7, 8, 9, 14, 15, 16, 17, 30, 31, 32, 33, 62, 63, 64, 65, 94, 95, 96, 126, 127,
];
/// Where a scan starts: unaligned to every step.
const FROMS: [usize; 10] = [0, 1, 7, 8, 15, 16, 31, 32, 63, 64];
const MAX_LEN: usize = 130;

fn control_oracle(buf: &[u8], from: usize) -> usize {
    (from..buf.len())
        .find(|&i| buf[i] < 0x20 || buf[i] == 0x7f)
        .unwrap_or(buf.len().max(from))
}

fn token_oracle(buf: &[u8]) -> TokenRun {
    const TCHARS: &[u8] =
        b"!#$%&'*+-.^_`|~0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let len = buf.iter().take_while(|b| TCHARS.contains(b)).count();
    let run = &buf[..len];
    TokenRun {
        len,
        upper: run.iter().any(u8::is_ascii_uppercase),
        underscore: run.contains(&b'_'),
    }
}

fn check_control(level: Level, buf: &[u8], from: usize) {
    assert_eq!(
        control_at_with(level, buf, from),
        control_oracle(buf, from),
        "{level:?} len={} from={from}",
        buf.len()
    );
}

fn check_token(level: Level, buf: &[u8]) {
    assert_eq!(
        token_run_with(level, buf),
        token_oracle(buf),
        "{level:?} {buf:?}"
    );
}

#[test]
fn is_tchar_is_the_rfc_9110_set() {
    let count = (0..=255u8).filter(|&b| is_tchar(b)).count();
    assert_eq!(count, 10 + 26 + 26 + 15);
    let tokens: Vec<u8> = (0..=255u8).filter(|&b| is_tchar(b)).collect();
    assert_eq!(token_oracle(&tokens).len, count);
}

#[test]
fn control_at_finds_every_byte_value_at_every_edge() {
    for level in Level::supported() {
        for len in 0..=MAX_LEN {
            let mut buf = vec![b'a'; len];
            for pos in EDGES.into_iter().filter(|&pos| pos < len) {
                for byte in 0..=255u8 {
                    buf[pos] = byte;
                    for from in FROMS.into_iter().filter(|&from| from <= len) {
                        check_control(level, &buf, from);
                    }
                }
                buf[pos] = b'a';
            }
        }
    }
}

#[test]
fn control_at_finds_the_representative_bytes_everywhere() {
    for level in Level::supported() {
        for len in 0..=MAX_LEN {
            let mut buf = vec![b' '; len];
            for pos in 0..len {
                for byte in [0x00, 0x09, 0x0d, 0x1f, 0x20, 0x7e, 0x7f, 0x80, 0xff] {
                    buf[pos] = byte;
                    check_control(level, &buf, 0);
                    check_control(level, &buf, pos);
                    check_control(level, &buf, pos + 1);
                }
                buf[pos] = b' ';
            }
        }
    }
}

#[test]
fn control_at_reports_the_first_of_several_hits() {
    for level in Level::supported() {
        let mut buf = vec![b'x'; MAX_LEN];
        for first in EDGES {
            for second in EDGES.into_iter().filter(|&second| second > first) {
                buf[first] = 0x0d;
                buf[second] = 0x7f;
                check_control(level, &buf, 0);
                buf[first] = b'x';
                buf[second] = b'x';
            }
        }
    }
}

#[test]
fn token_run_handles_every_byte_value_at_every_edge() {
    for level in Level::supported() {
        for len in 0..=MAX_LEN {
            let mut buf = vec![b'a'; len];
            for pos in EDGES.into_iter().filter(|&pos| pos < len) {
                for byte in 0..=255u8 {
                    buf[pos] = byte;
                    check_token(level, &buf);
                }
                buf[pos] = b'a';
            }
        }
    }
}

#[test]
fn token_run_handles_the_representative_bytes_everywhere() {
    for level in Level::supported() {
        for len in 0..=MAX_LEN {
            let mut buf = vec![b'a'; len];
            for pos in 0..len {
                for byte in [
                    0x00, b' ', b':', b'@', b'A', b'Z', b'[', b'^', b'_', b'`', b'z', b'{', b'~',
                    0x7f, 0x80, 0xff,
                ] {
                    buf[pos] = byte;
                    check_token(level, &buf);
                }
                buf[pos] = b'a';
            }
            check_token(level, &buf);
        }
    }
}

#[test]
fn token_run_counts_capitals_and_underscores_only_inside_the_run() {
    for level in Level::supported() {
        let mut buf = vec![b'a'; MAX_LEN];
        for stop in EDGES {
            for mark in EDGES {
                for (stopper, marker) in [
                    (b' ', b'A'),
                    (b':', b'_'),
                    (0x00, b'Z'),
                    (0xc1, b'_'),
                    (b'@', b'A'),
                    (b'[', b'Z'),
                ] {
                    buf[stop] = stopper;
                    buf[mark] = if mark == stop { stopper } else { marker };
                    check_token(level, &buf);
                    check_token(level, &buf[..stop.max(mark)]);
                    buf[stop] = b'a';
                    buf[mark] = b'a';
                }
            }
        }
    }
}

#[test]
fn token_run_of_a_whole_buffer_of_tokens() {
    for level in Level::supported() {
        for len in 0..=MAX_LEN {
            check_token(level, &vec![b'-'; len]);
            check_token(level, &vec![b'Q'; len]);
            check_token(level, &vec![b'_'; len]);
        }
    }
}

#[test]
fn levels_above_the_hardware_are_clamped() {
    let buf = b"X-Forwarded-For: 192.0.2.1\r\nHost: example.com\r\n".repeat(3);
    for level in Level::ALL {
        for from in 0..buf.len() {
            check_control(level, &buf, from);
        }
        for from in 0..buf.len() {
            check_token(level, &buf[from..]);
        }
    }
}
