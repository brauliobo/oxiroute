//! Cost of each scanner at each level, in ns per call and ns per byte, on inputs of header sizes.
//!
//! `cargo run --release -p oxiroute-cpu --example scanbench`

use oxiroute_cpu::{Job, Lanes, Level, control_at_with, dispatch_with};
use std::hint::black_box;
use std::time::Instant;

const LENS: [usize; 9] = [4, 8, 16, 24, 32, 48, 64, 128, 512];
/// Bytes of the rest of the head after the scanned one, as in a real buffer.
const TRAILING: usize = 64;
const ROUNDS: u32 = 2_000_000;

/// The length of the token run at the start of the buffer, a window at a time.
struct NameRun<'a>(&'a [u8]);

impl Job for NameRun<'_> {
    type Out = usize;

    #[inline(always)]
    fn run<L: Lanes>(self, lanes: L) -> usize {
        let mut len = 0;
        loop {
            let stop = lanes.token(&lanes.window(self.0, len)).stop;
            len += stop.trailing_zeros() as usize;
            if stop != 0 {
                return len;
            }
        }
    }
}

fn per_call(mut f: impl FnMut() -> usize) -> f64 {
    for _ in 0..ROUNDS / 10 {
        black_box(f());
    }
    let best = (0..5)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..ROUNDS {
                black_box(f());
            }
            start.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min);
    best * 1e9 / f64::from(ROUNDS)
}

fn main() {
    println!(
        "hardware {:?}; ns per call (ns per byte) of the best of 5 runs of {ROUNDS} calls",
        Level::hardware()
    );
    for (scanner, filler, stop) in [("control_at", b'v', b"\r"), ("name_run", b'n', b":")] {
        println!("{scanner}");
        print!("{:>8}", "len");
        for level in Level::supported() {
            print!("{:>16}", level.name());
        }
        println!();
        for len in LENS {
            let mut buf = vec![filler; len + 1 + TRAILING];
            buf[len] = stop[0];
            buf[len / 3] = if scanner == "name_run" { b'-' } else { filler };
            print!("{len:>8}");
            for level in Level::supported() {
                let ns = match scanner {
                    "control_at" => per_call(|| control_at_with(level, black_box(&buf), 0)),
                    _ => per_call(|| dispatch_with(level, NameRun(black_box(&buf)))),
                };
                print!("{:>16}", format!("{ns:.1} ({:.3})", ns / len as f64));
            }
            println!();
        }
    }
}
