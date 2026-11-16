//! The portable scanners: the fallback of every level and the tail of the vector ones.

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
