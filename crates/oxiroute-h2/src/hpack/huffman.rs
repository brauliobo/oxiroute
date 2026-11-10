//! HPACK Huffman coding (RFC 7541 section 5.2).
use super::huff_table::{CODES, DECODE, EOS, LENGTHS, POINTER};

/// Bytes the Huffman form of `src` takes.
pub(super) fn encoded_len(src: &[u8]) -> usize {
    let bits: usize = src
        .iter()
        .map(|&b| usize::from(LENGTHS[usize::from(b)]))
        .sum();
    bits.div_ceil(8)
}

/// Appends the Huffman form of `src`, padded with the most significant bits of EOS.
pub(super) fn encode(src: &[u8], out: &mut Vec<u8>) {
    let (mut acc, mut bits) = (0u64, 0u32);
    for &b in src {
        let (code, len) = (CODES[usize::from(b)], u32::from(LENGTHS[usize::from(b)]));
        acc = acc << len | u64::from(code);
        bits += len;
        if bits >= 32 {
            bits -= 32;
            out.extend_from_slice(&((acc >> bits) as u32).to_be_bytes());
        }
    }
    while bits >= 8 {
        bits -= 8;
        out.push((acc >> bits) as u8);
    }
    if bits > 0 {
        out.push((acc << (8 - bits)) as u8 | (0xff >> bits));
    }
}

/// Destination of decoded octets: the bytes themselves or just their count.
pub(super) trait Sink {
    fn put(&mut self, byte: u8);
}

impl Sink for Vec<u8> {
    #[inline]
    fn put(&mut self, byte: u8) {
        self.push(byte);
    }
}

pub(super) struct Count(pub usize);

impl Sink for Count {
    #[inline]
    fn put(&mut self, _: u8) {
        self.0 += 1;
    }
}

/// Octets the Huffman input `src` decodes to at most (the shortest code is five bits).
pub(super) const fn max_decoded_len(src_len: usize) -> usize {
    src_len.saturating_mul(8) / 5
}

/// The input is not valid Huffman data: EOS inside the data, or padding that is not at most seven EOS bits.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Invalid;

/// Decodes `src` into `sink`.
pub(super) fn decode(src: &[u8], sink: &mut impl Sink) -> Result<(), Invalid> {
    let (mut acc, mut bits, mut rest) = (0u64, 0u32, src);
    loop {
        if bits < 32 {
            (acc, bits, rest) = refill(acc, bits, rest);
        }
        if bits == 0 {
            return Ok(());
        }
        let mut entry = DECODE[0][(acc >> 56) as usize];
        let mut skipped = 0;
        while entry & POINTER != 0 {
            skipped += 8;
            entry = DECODE[usize::from(entry & 0xff)][((acc << skipped) >> 56) as usize];
        }
        let len = u32::from(entry >> 8 & 15) + skipped;
        if len > bits || entry & EOS != 0 {
            return padding(acc, bits, entry & EOS == 0);
        }
        sink.put(entry as u8);
        acc <<= len;
        bits -= len;
    }
}

/// What is left must be fewer than eight bits, all ones.
fn padding(acc: u64, bits: u32, truncated: bool) -> Result<(), Invalid> {
    if truncated && bits < 8 && acc >> (64 - bits) == (1 << bits) - 1 {
        Ok(())
    } else {
        Err(Invalid)
    }
}

#[inline]
fn refill(mut acc: u64, mut bits: u32, rest: &[u8]) -> (u64, u32, &[u8]) {
    if let Some((word, tail)) = rest.split_first_chunk::<4>() {
        return (
            acc | u64::from(u32::from_be_bytes(*word)) << (32 - bits),
            bits + 32,
            tail,
        );
    }
    let mut rest = rest;
    while bits <= 56 {
        let Some((&byte, tail)) = rest.split_first() else {
            break;
        };
        acc |= u64::from(byte) << (56 - bits);
        bits += 8;
        rest = tail;
    }
    (acc, bits, rest)
}

#[cfg(test)]
mod tests;
