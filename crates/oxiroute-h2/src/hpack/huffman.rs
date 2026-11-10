//! HPACK Huffman coding (RFC 7541 section 5.2): 8-bit first-level lookup, tree walk for the rare longer codes.
use super::huff_table::{ENCODE_CODE_LENGTHS, ENCODE_CODES};
use std::sync::OnceLock;

struct Tables {
    /// per 8-bit prefix: symbol | len << 16, 0 when the code is longer than 8 bits
    fast: [u32; 256],
    /// binary trie: node -> [child0, child1]; values >= 0x8000 are leaves (symbol | 0x8000)
    tree: Vec<[u16; 2]>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut tree = vec![[0u16; 2]];
        for sym in 0..256usize {
            let (code, len) = (ENCODE_CODES[sym], ENCODE_CODE_LENGTHS[sym] as u32);
            let mut node = 0usize;
            for i in (0..len).rev() {
                let bit = ((code >> i) & 1) as usize;
                if i == 0 {
                    tree[node][bit] = 0x8000 | sym as u16;
                } else {
                    if tree[node][bit] == 0 {
                        tree.push([0, 0]);
                        tree[node][bit] = (tree.len() - 1) as u16;
                    }
                    node = tree[node][bit] as usize;
                }
            }
        }
        let mut fast = [0u32; 256];
        for (prefix, slot) in fast.iter_mut().enumerate() {
            let mut node = 0usize;
            for i in 0..8 {
                let next = tree[node][(prefix >> (7 - i)) & 1];
                if next & 0x8000 != 0 {
                    *slot = (next & 0xff) as u32 | (i as u32 + 1) << 16;
                    break;
                }
                if next == 0 {
                    break;
                }
                node = next as usize;
            }
        }
        Tables { fast, tree }
    })
}

pub fn encoded_len(src: &[u8]) -> usize {
    (src.iter()
        .map(|&b| ENCODE_CODE_LENGTHS[b as usize] as usize)
        .sum::<usize>()
        + 7)
        / 8
}

pub fn encode(src: &[u8], out: &mut Vec<u8>) {
    let (mut acc, mut n) = (0u64, 0u32);
    for &b in src {
        let len = ENCODE_CODE_LENGTHS[b as usize] as u32;
        acc = acc << len | ENCODE_CODES[b as usize] as u64;
        n += len;
        while n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
        }
    }
    if n > 0 {
        out.push(((acc << (8 - n)) as u8) | (0xff >> n));
    }
}

pub fn decode(src: &[u8], out: &mut Vec<u8>) -> Result<(), ()> {
    let t = tables();
    let (mut acc, mut n) = (0u64, 0u32);
    let mut it = src.iter();
    loop {
        while n <= 56 {
            match it.next() {
                Some(&b) => {
                    acc = acc << 8 | b as u64;
                    n += 8;
                }
                None => break,
            }
        }
        if n == 0 {
            return Ok(());
        }
        let peek = if n >= 8 {
            (acc >> (n - 8)) as usize & 0xff
        } else {
            ((acc << (8 - n)) as usize | (0xff >> n)) & 0xff
        };
        let e = t.fast[peek];
        let len = e >> 16;
        if len != 0 && len <= n {
            out.push(e as u8);
            n -= len;
            continue;
        }
        if n < 8 {
            // only padding may remain: a prefix of EOS (all ones), shorter than one byte
            return if acc & ((1 << n) - 1) == (1 << n) - 1 {
                Ok(())
            } else {
                Err(())
            };
        }
        let mut node = 0usize;
        let mut used = 0;
        loop {
            if used == n {
                return if n < 8 && acc & ((1 << n) - 1) == (1 << n) - 1 {
                    Ok(())
                } else {
                    Err(())
                };
            }
            used += 1;
            let next = t.tree[node][((acc >> (n - used)) & 1) as usize];
            if next & 0x8000 != 0 {
                out.push(next as u8);
                n -= used;
                break;
            }
            if next == 0 {
                return Err(());
            }
            node = next as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_vector() {
        // RFC 7541 C.4.1: "www.example.com"
        let enc = [
            0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
        ];
        let mut out = vec![];
        decode(&enc, &mut out).unwrap();
        assert_eq!(out, b"www.example.com");
        let mut e = vec![];
        encode(b"www.example.com", &mut e);
        assert_eq!(e, enc);
        assert_eq!(encoded_len(b"www.example.com"), enc.len());
    }

    #[test]
    fn all_octets_and_lengths() {
        for len in 0..300usize {
            let src: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
            let mut e = vec![];
            encode(&src, &mut e);
            let mut d = vec![];
            decode(&e, &mut d).unwrap();
            assert_eq!(d, src, "len {len}");
        }
    }
}
