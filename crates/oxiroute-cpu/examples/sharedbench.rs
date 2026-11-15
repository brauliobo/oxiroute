//! Actual public API dispatch against the former caller loops; each level runs in its own process.
//!
//! `cargo run --release -p oxiroute-cpu --example sharedbench > sharedbench.csv`

use oxiroute_cpu::{
    ENV, Level, ascii_range, control_at, find_any3, header_value_end, json_escape_at, level,
};
use std::hint::black_box;
use std::process::Command;
use std::time::Instant;

const ROUNDS: usize = 20_000;
const SAMPLES: usize = 7;

fn median(mut f: impl FnMut() -> usize) -> (f64, f64, f64) {
    for _ in 0..ROUNDS / 10 {
        black_box(f());
    }
    let mut samples = [0.0; SAMPLES];
    for sample in &mut samples {
        let start = Instant::now();
        for _ in 0..ROUNDS {
            black_box(f());
        }
        *sample = start.elapsed().as_secs_f64() * 1e9 / ROUNDS as f64;
    }
    samples.sort_by(f64::total_cmp);
    (samples[SAMPLES / 2], samples[0], samples[SAMPLES - 1])
}

fn compare(
    operation: &str,
    shape: &str,
    buf: &[u8],
    old: impl Fn(&[u8]) -> usize,
    new: impl Fn(&[u8]) -> usize,
) {
    let (old, old_min, old_max) = median(|| old(black_box(buf)));
    let (new, new_min, new_max) = median(|| new(black_box(buf)));
    println!(
        "{},{operation},{shape},{},{old:.3},{new:.3},{old_min:.3},{old_max:.3},{new_min:.3},{new_max:.3}",
        level().name(),
        buf.len()
    );
}

fn sample() {
    for len in [4, 8, 16, 24, 32, 64, 128, 512, 4096] {
        for shape in ["absent", "first", "last", "tabs"] {
            let mut buf = vec![if shape == "tabs" { b'\t' } else { b'a' }; len];
            if matches!(shape, "first" | "last") {
                buf[if shape == "first" { 0 } else { len - 1 }] = 0;
            }
            compare(
                "find_any3",
                shape,
                &buf,
                |buf| {
                    buf.iter()
                        .position(|&b| matches!(b, 0 | b'%' | b'\\'))
                        .unwrap_or(buf.len())
                },
                |buf| find_any3(buf, 0, [0, b'%', b'\\']),
            );
            compare(
                "ascii_range",
                shape,
                &buf,
                |buf| usize::from(buf.iter().all(|&b| (32..=126).contains(&b))),
                |buf| usize::from(ascii_range(buf, 32, 126)),
            );
            compare(
                "json_escape",
                shape,
                &buf,
                |buf| {
                    buf.iter()
                        .position(|&b| b < 32 || matches!(b, b'"' | b'\\'))
                        .unwrap_or(buf.len())
                },
                json_escape_at,
            );
            compare(
                "header_value",
                shape,
                &buf,
                |buf| {
                    let mut end = control_at(buf, 0);
                    while end < buf.len() && buf[end] == b'\t' {
                        end = control_at(buf, end + 1);
                    }
                    end
                },
                |buf| header_value_end(buf, 0),
            );
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--sample") {
        sample();
        return;
    }
    println!(
        "level,operation,shape,bytes,old_ns,new_ns,old_min_ns,old_max_ns,new_min_ns,new_max_ns"
    );
    for level in Level::supported() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--sample")
            .env(ENV, level.name())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        print!("{}", String::from_utf8(output.stdout).unwrap());
    }
}
