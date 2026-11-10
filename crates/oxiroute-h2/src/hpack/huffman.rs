//! HPACK Huffman coding (RFC 7541 section 5.2).
use super::huff_table::{CODES, DECODE, EOS, LENGTHS, POINTER};

/// Longest code, in bits.
const MAX_CODE_LEN: u32 = 30;

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
        (acc, bits, rest) = refill(acc, bits, rest);
        if bits < MAX_CODE_LEN {
            return tail(acc, bits, sink);
        }
        while bits >= MAX_CODE_LEN {
            let entry = DECODE[0][(acc >> 56) as usize];
            let (symbol, len) = match entry >> 12 {
                0 => walk(acc).ok_or(Invalid)?,
                len => (entry as u8, u32::from(len)),
            };
            sink.put(symbol);
            acc <<= len;
            bits -= len;
        }
    }
}

/// With fewer than 30 bits left a code may run past the end: what remains must be a prefix of EOS.
fn tail(mut acc: u64, mut bits: u32, sink: &mut impl Sink) -> Result<(), Invalid> {
    while bits > 0 {
        match walk(acc) {
            Some((symbol, len)) if len <= bits => {
                sink.put(symbol);
                acc <<= len;
                bits -= len;
            }
            Some(_) if bits < 8 && acc >> (64 - bits) == (1 << bits) - 1 => return Ok(()),
            _ => return Err(Invalid),
        }
    }
    Ok(())
}

/// The symbol and length of the code at the top of `acc`, `None` for EOS. Bits past the input read as zero.
fn walk(acc: u64) -> Option<(u8, u32)> {
    let mut entry = DECODE[0][(acc >> 56) as usize];
    let mut skipped = 0;
    while entry & POINTER != 0 {
        skipped += 8;
        entry = DECODE[usize::from(entry & 0xff)][((acc << skipped) >> 56) as usize];
    }
    (entry & EOS == 0).then_some((entry as u8, u32::from(entry >> 12) + skipped))
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
