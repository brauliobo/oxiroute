//! The portable scanners: the fallback of every level and the tail of the vector ones.

use crate::TokenRun;

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

pub(crate) fn control_at(buf: &[u8], mut i: usize) -> usize {
    while let Some(chunk) = buf.get(i..).and_then(<[u8]>::first_chunk::<8>) {
        let x = u64::from_le_bytes(*chunk);
        let below = x.wrapping_sub(LO * 0x20) & !x & HI;
        let del = x ^ (LO * 0x7f);
        let found = below | (del.wrapping_sub(LO) & !del & HI);
        if found != 0 {
            return i + (found.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < buf.len() && buf[i] >= 0x20 && buf[i] != 0x7f {
        i += 1;
    }
    i
}

static TCHAR: [bool; 256] = {
    let mut table = [false; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = crate::is_tchar(i as u8);
        i += 1;
    }
    table
};

pub(crate) fn token_run(buf: &[u8]) -> TokenRun {
    let len = buf.iter().take_while(|&&b| TCHAR[usize::from(b)]).count();
    let run = &buf[..len];
    TokenRun {
        len,
        upper: run.iter().any(u8::is_ascii_uppercase),
        underscore: run.contains(&b'_'),
    }
}
