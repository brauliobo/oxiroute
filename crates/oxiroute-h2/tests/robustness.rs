//! Deterministic robustness run: frame soup and mutated valid scripts must never panic the connection and the
//! output must stay parseable (the same harness backs the `h2_frames` fuzz target).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::similar_names
)]
mod harness;
mod support;

use support::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// `ROBUSTNESS_SCALE=n` multiplies the number of generated inputs for a longer local run.
fn rounds(base: usize) -> usize {
    base * std::env::var("ROBUSTNESS_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

fn valid_script() -> Vec<u8> {
    let mut c = Client::new();
    let mut s = vec![];
    s.extend(settings(&[(4, 70_000), (5, 20_000)]));
    s.extend(settings_ack());
    s.extend(c.get(1, "/a"));
    s.extend(c.post(3, "/b"));
    s.extend(data(3, &[1; 500], 0));
    s.extend(data(3, &[2; 100], END_STREAM));
    s.extend(ping([1; 8], false));
    s.extend(window_update(0, 1000));
    s.extend(window_update(3, 10));
    s.extend(priority(5, 3));
    s.extend(c.get(5, "/c"));
    s.extend(rst(5, 8));
    let block = c.get_block("/long");
    s.extend(headers(7, &block[..block.len() / 2], END_STREAM));
    s.extend(continuation(7, &block[block.len() / 2..], END_HEADERS));
    s.extend(c.post(9, "/d"));
    s.extend(data(9, &[0; 3000], 0));
    s.extend(rst(9, 0));
    s.extend(goaway(0, 0));
    s
}

fn soup(rng: &mut Rng) -> Vec<u8> {
    let mut c = Client::new();
    let mut out = settings(&[]);
    let mut next = 1;
    for _ in 0..rng.below(40) {
        let kind = if rng.below(4) == 0 {
            rng.below(256) as u8
        } else {
            rng.below(10) as u8
        };
        let len = if rng.below(8) == 0 {
            rng.below(3000)
        } else {
            rng.below(40)
        };
        let mut stream = match rng.below(6) {
            0 => 0,
            1 => rng.next() as u32,
            _ => 1 + 2 * rng.below(8) as u32,
        };
        if kind == HEADERS && rng.below(4) != 0 {
            stream = next;
            next += 2;
        }
        let payload = match (kind, rng.below(2)) {
            (HEADERS, 0) => c.get_block("/x"),
            _ => rng.bytes(len),
        };
        out.extend(frame(kind, rng.next() as u8, stream, &payload));
    }
    out
}

/// Mostly well-formed traffic with occasional corrupted frames.
fn structured(rng: &mut Rng) -> Vec<u8> {
    let mut c = Client::new();
    let mut out = settings(&[]);
    let mut next = 1u32;
    for _ in 0..rng.below(60) {
        let open = 1 + 2 * rng.below(next as usize / 2 + 1) as u32;
        let mut f = match rng.below(9) {
            0..=2 => {
                next += 2;
                c.get(next - 2, "/s")
            }
            3 => {
                next += 2;
                c.post(next - 2, "/p")
            }
            4 => {
                let n = rng.below(400);
                data(open, &rng.bytes(n), (rng.below(3) as u8) & 1)
            }
            5 => ping([rng.next() as u8; 8], rng.below(2) == 0),
            6 => window_update(
                if rng.below(2) == 0 { 0 } else { open },
                1 + rng.below(5000) as u32,
            ),
            7 => rst(open, rng.below(14) as u32),
            _ => priority(open, open + 2),
        };
        if rng.below(12) == 0 {
            let at = rng.below(f.len());
            f[at] = rng.next() as u8;
        }
        out.extend(f);
    }
    out
}

fn mutate(rng: &mut Rng, mut s: Vec<u8>) -> Vec<u8> {
    for _ in 0..=rng.below(6) {
        let at = rng.below(s.len());
        match rng.below(4) {
            0 => s[at] ^= 1 << rng.below(8),
            1 => s[at] = rng.next() as u8,
            2 => {
                s.remove(at);
            }
            _ => s.insert(at, rng.next() as u8),
        }
    }
    s
}

fn with_header(rng: &mut Rng, body: Vec<u8>) -> Vec<u8> {
    let mut v = vec![(rng.next() as u8 & !1) | 0x20, rng.next() as u8];
    v.extend(body);
    v
}

#[test]
fn random_input_never_panics() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..rounds(1500) {
        let n = rng.below(600);
        let body = rng.bytes(n);
        harness::run(&with_header(&mut rng, body));
    }
}

#[test]
fn frame_soup_never_panics() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut accepted = 0;
    for _ in 0..rounds(3000) {
        let body = soup(&mut rng);
        accepted += harness::run(&with_header(&mut rng, body));
    }
    assert!(accepted > 0, "soup should sometimes reach the request path");
}

#[test]
fn structured_traffic_never_panics() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut accepted = 0;
    for _ in 0..rounds(3000) {
        let body = structured(&mut rng);
        accepted += harness::run(&with_header(&mut rng, body));
    }
    assert!(
        accepted > 1500,
        "structured traffic should reach the request path, accepted {accepted}"
    );
}

#[test]
fn mutated_valid_scripts_never_panic() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let valid = valid_script();
    let clean = [14, 255]
        .into_iter()
        .chain(valid.clone())
        .collect::<Vec<_>>();
    assert!(harness::run(&clean) >= 4, "the valid script is accepted");
    let mut accepted = 0;
    for _ in 0..rounds(4000) {
        let body = mutate(&mut rng, valid.clone());
        accepted += harness::run(&with_header(&mut rng, body));
    }
    assert!(
        accepted > 4000,
        "mutations should mostly stay valid, accepted {accepted}"
    );
}

#[test]
fn fuzz_corpus_seeds_run_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/h2_frames");
    let mut accepted = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        let hex = text.trim().strip_prefix("hex:").unwrap();
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        accepted += harness::run(&bytes);
    }
    assert!(
        accepted >= 5,
        "the seeds should reach the request path, accepted {accepted}"
    );
}
