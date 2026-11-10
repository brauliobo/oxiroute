//! Huffman code of RFC 7541 Appendix B, built at compile time.
//!
//! The code is canonical: symbols sorted by (length, value) get consecutive code values, so the 257 code lengths
//! determine the table. The decode tables are a 256-way trie, eight bits per level.

/// Code length in bits per symbol; index 256 is EOS.
pub(super) const LENGTHS: [u8; 257] = [
    13, 23, 28, 28, 28, 28, 28, 28, 28, 24, 30, 28, 28, 30, 28, 28, 28, 28, 28, 28, 28, 28, 30, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 6, 10, 10, 12, 13, 6, 8, 11, 10, 10, 8, 11, 8, 6, 6, 6, 5, 5,
    5, 6, 6, 6, 6, 6, 6, 6, 7, 8, 15, 6, 12, 10, 13, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 8, 7, 8, 13, 19, 13, 14, 6, 15, 5, 6, 5, 6, 5, 6, 6, 6, 5, 7, 7, 6, 6,
    6, 5, 6, 7, 6, 5, 5, 6, 7, 7, 7, 7, 7, 15, 11, 14, 13, 28, 20, 22, 20, 20, 22, 22, 22, 23, 22,
    23, 23, 23, 23, 23, 24, 23, 24, 24, 22, 23, 24, 23, 23, 23, 23, 21, 22, 23, 22, 23, 23, 24, 22,
    21, 20, 22, 22, 23, 23, 21, 23, 22, 22, 24, 21, 22, 23, 23, 21, 21, 22, 21, 23, 22, 23, 23, 20,
    22, 22, 22, 23, 22, 22, 23, 26, 26, 20, 19, 22, 23, 22, 25, 26, 26, 26, 27, 27, 26, 24, 25, 19,
    21, 26, 27, 27, 26, 27, 24, 21, 21, 26, 26, 28, 27, 27, 27, 20, 24, 20, 21, 22, 21, 21, 23, 22,
    22, 25, 25, 24, 24, 26, 23, 26, 27, 26, 26, 27, 27, 27, 27, 27, 28, 27, 27, 27, 27, 27, 26, 30,
];

/// Code value (right-aligned) per symbol.
pub(super) static CODES: [u32; 257] = canonical_codes();

const fn canonical_codes() -> [u32; 257] {
    let mut codes = [0u32; 257];
    let mut code = 0u32;
    let mut len = 1;
    while len <= 30 {
        let mut sym = 0;
        while sym < 257 {
            if LENGTHS[sym] as usize == len {
                codes[sym] = code;
                code += 1;
            }
            sym += 1;
        }
        code <<= 1;
        len += 1;
    }
    codes
}

/// Entry: bits 0..8 symbol or next table, bits 8..12 code bits consumed at this level.
pub(super) const POINTER: u16 = 1 << 12;
pub(super) const EOS: u16 = 1 << 13;
const TABLES: usize = 15;

pub(super) type Level = [u16; 256];

/// Level 0 is the root. A leaf stores the bits it consumes at its level and is replicated over every 8-bit
/// continuation of its code.
pub(super) static DECODE: [Level; TABLES] = build_decode();

const fn build_decode() -> [Level; TABLES] {
    let mut tables = [[0u16; 256]; TABLES];
    let mut used = 1;
    let mut sym = 0;
    while sym < 257 {
        let (code, len) = (CODES[sym], LENGTHS[sym] as u32);
        let (mut table, mut left) = (0, len);
        while left > 8 {
            let index = ((code >> (left - 8)) & 0xff) as usize;
            if tables[table][index] == 0 {
                tables[table][index] = POINTER | used as u16;
                used += 1;
            }
            table = (tables[table][index] & 0xff) as usize;
            left -= 8;
        }
        let leaf = (left as u16) << 8 | if sym == 256 { EOS } else { sym as u16 };
        let first = ((code & ((1 << left) - 1)) << (8 - left)) as usize;
        let mut slot = 0;
        while slot < 1 << (8 - left) {
            tables[table][first + slot] = leaf;
            slot += 1;
        }
        sym += 1;
    }
    assert!(used == TABLES);
    tables
}

#[cfg(test)]
mod rfc_table;
