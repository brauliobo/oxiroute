//! Every level the CPU runs against a lane-at-a-time oracle: one window at every offset of buffers shorter and longer than
//! a window, on every byte value, so the lanes past the end and the tail windows are covered too.

use oxiroute_cpu::{
    Job, Lanes, Level, TokenMasks, WIDTH, dispatch_with, is_target_byte, is_tchar,
};

/// Positions at the ends of a window and of the buffers.
const EDGES: [usize; 8] = [0, 1, 15, 16, 30, 31, 32, 33];

#[derive(Debug, PartialEq, Eq)]
struct Answer {
    token: TokenMasks,
    control: u32,
    target: u32,
}

struct Probe<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Job for Probe<'_> {
    type Out = Answer;

    #[inline(always)]
    fn run<L: Lanes>(self, lanes: L) -> Answer {
        let window = lanes.window(self.buf, self.at);
        Answer {
            token: lanes.token(&window),
            control: lanes.control_mask(&window),
            target: lanes.target_mask(&window),
        }
    }
}

fn oracle(buf: &[u8], at: usize) -> Answer {
    let mut answer = Answer {
        token: TokenMasks {
            stop: 0,
            upper: 0,
            underscore: 0,
        },
        control: 0,
        target: 0,
    };
    for lane in 0..WIDTH {
        let byte = buf.get(at + lane).copied();
        let bit = 1u32 << lane;
        answer.token.stop |= bit * u32::from(!byte.is_some_and(is_tchar));
        answer.token.upper |= bit * u32::from(byte.is_some_and(|b| b.is_ascii_uppercase()));
        answer.token.underscore |= bit * u32::from(byte == Some(b'_'));
        answer.control |= bit * u32::from(byte.is_none_or(|b| b < 0x20 || b == 0x7f));
        answer.target |= bit * u32::from(!byte.is_some_and(is_target_byte));
    }
    answer
}

fn check(level: Level, buf: &[u8], at: usize) {
    assert_eq!(
        dispatch_with(level, Probe { buf, at }),
        oracle(buf, at),
        "{level:?} len={} at={at}",
        buf.len()
    );
}

fn check_every_offset(level: Level, buf: &[u8]) {
    for at in 0..=buf.len() + 1 {
        check(level, buf, at);
    }
}

#[test]
fn is_target_byte_is_what_http_uri_stores_unchanged() {
    let allowed: Vec<u8> = (0..=255u8).filter(|&b| is_target_byte(b)).collect();
    assert_eq!(allowed.len(), 1 + 0x3b - 0x24 + 1 + 1 + 0x5f - 0x3f + 1 + 0x7e - 0x61 + 1);
    for forbidden in [b' ', b'"', b'#', b'<', b'>', b'`', 0x00, 0x7f, 0x80, 0xff] {
        assert!(!is_target_byte(forbidden));
    }
}

#[test]
fn every_byte_value_at_every_position_of_short_and_long_buffers() {
    for level in Level::supported() {
        for len in [0, 1, 5, 31, 32, 33, 40, 65] {
            let mut buf = vec![b'a'; len];
            for pos in (0..len).filter(|pos| EDGES.contains(pos) || pos + 1 == len) {
                for byte in 0..=255u8 {
                    buf[pos] = byte;
                    check_every_offset(level, &buf);
                }
                buf[pos] = b'a';
            }
        }
    }
}

#[test]
fn whole_buffers_of_one_class_at_every_length() {
    for level in Level::supported() {
        for len in 0..=100 {
            for fill in [b'-', b'Q', b'_', b':', b' ', b'\r', 0x00, 0x7f, b'/', b'?', 0xc3] {
                check_every_offset(level, &vec![fill; len]);
            }
        }
    }
}

#[test]
fn a_head_at_every_offset() {
    let head = b"GET /index.html?a=1&b=%20 HTTP/1.1\r\nHost: example.com\r\nX_Odd-Name: v\talue\r\n\r\n";
    for level in Level::supported() {
        check_every_offset(level, head);
    }
}

#[test]
fn levels_above_the_hardware_are_clamped() {
    let head = b"X-Forwarded-For: 192.0.2.1\r\nHost: example.com\r\n".repeat(3);
    for level in Level::ALL {
        check_every_offset(level, &head);
    }
}
