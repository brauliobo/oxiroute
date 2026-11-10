//! Shared test support: a deterministic PRNG and the classic request path reimplemented from the vendored sources
//! (`read_request`: httparse, `RequestHeader::build_no_case`, one `append` per header, `validate_request`).

#![allow(dead_code)]

pub mod classic_request;
pub mod classic_write;
pub mod request_corpus;

/// xorshift64*: deterministic, seedable, no dependency.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// One mutation of a seed head: replace, insert or delete a byte, or duplicate a line.
pub fn mutate(rng: &mut Rng, buf: &mut Vec<u8>, alphabet: &[u8]) {
    for _ in 0..=rng.below(3) {
        if buf.is_empty() {
            return;
        }
        let at = rng.below(buf.len());
        match rng.below(5) {
            0 => buf[at] = *rng.pick(alphabet),
            1 => buf.insert(at, *rng.pick(alphabet)),
            2 => {
                buf.remove(at);
            }
            3 => {
                let start = buf[..at]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |p| p + 1);
                let end = buf[at..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(buf.len(), |p| at + p + 1);
                let line = buf[start..end].to_vec();
                buf.splice(end..end, line);
            }
            _ => {
                let to = rng.below(buf.len());
                buf.swap(at, to);
            }
        }
    }
}

pub const MUTATION_ALPHABET: &[u8] =
    b" \t\r\n:/?%#\x00\x01\x1f\x7f\x80\xffaZ09-_.,;\"HTP=*@{}[]\\<>`^|~";
