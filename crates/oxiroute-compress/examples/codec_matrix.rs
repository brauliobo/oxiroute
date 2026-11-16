use std::{
    hint::black_box,
    io::Read,
    time::{Duration, Instant},
};

use oxiroute_compress::{Coding, Levels, compress};

const BODY_BYTES: usize = 64 * 1024;
const ROUNDS: u32 = 5;
const BUDGET: Duration = Duration::from_millis(300);
const GZIP_LEVELS: [u8; 5] = [1, 3, 6, 9, 12];
const BROTLI_LEVELS: [u8; 5] = [3, 4, 5, 9, 11];
const ZSTD_LEVELS: [i8; 6] = [1, 3, 5, 7, 9, 19];

fn main() {
    println!(
        "corpus,codec,level,bytes,ratio,micros_per_body,mb_per_s,cpu_pressure_before,cpu_pressure_after"
    );
    for path in std::env::args().skip(1) {
        let mut body = std::fs::read(&path).expect("corpus file is readable");
        body.truncate(BODY_BYTES);
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for (coding, level) in contenders() {
            row(&name, &body, coding, level);
        }
    }
}

fn contenders() -> Vec<(Coding, i32)> {
    let gzip = GZIP_LEVELS
        .iter()
        .map(|level| (Coding::Gzip, i32::from(*level)));
    let brotli = BROTLI_LEVELS
        .iter()
        .map(|level| (Coding::Brotli, i32::from(*level)));
    let zstd = ZSTD_LEVELS
        .iter()
        .map(|level| (Coding::Zstd, i32::from(*level)));
    gzip.chain(brotli).chain(zstd).collect()
}

fn levels(coding: Coding, level: i32) -> Levels {
    let mut levels = Levels::default();
    match coding {
        Coding::Gzip => levels.gzip = u8::try_from(level).unwrap(),
        Coding::Brotli => levels.brotli = u8::try_from(level).unwrap(),
        Coding::Zstd => levels.zstd = i8::try_from(level).unwrap(),
    }
    levels
}

fn row(corpus: &str, body: &[u8], coding: Coding, level: i32) {
    let levels = levels(coding, level);
    let encoded = compress(coding, levels, body).unwrap();
    assert_eq!(decode(coding, &encoded), body, "{coding:?} round trip");
    let before = pressure();
    let micros = time(|| compress(coding, levels, black_box(body)).unwrap().len());
    let after = pressure();
    let input = f64::from(u32::try_from(body.len()).unwrap());
    let output = f64::from(u32::try_from(encoded.len()).unwrap());
    println!(
        "{corpus},{},{level},{},{:.3},{micros:.1},{:.0},{before},{after}",
        coding.token(),
        encoded.len(),
        input / output,
        input / micros,
    );
}

fn pressure() -> String {
    let text = std::fs::read_to_string("/proc/pressure/cpu").unwrap_or_default();
    text.lines()
        .find(|line| line.starts_with("some"))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("avg10="))
        })
        .unwrap_or("n/a")
        .to_owned()
}

fn time(mut run: impl FnMut() -> usize) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..ROUNDS {
        let started = Instant::now();
        let mut iterations = 0u32;
        while started.elapsed() < BUDGET {
            black_box(run());
            iterations += 1;
        }
        best = best.min(started.elapsed().as_secs_f64() * 1e6 / f64::from(iterations));
    }
    best
}

fn decode(coding: Coding, encoded: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    match coding {
        Coding::Gzip => {
            flate2::read::GzDecoder::new(encoded)
                .read_to_end(&mut out)
                .unwrap();
        }
        Coding::Brotli => {
            brotli::Decompressor::new(encoded, 4096)
                .read_to_end(&mut out)
                .unwrap();
        }
        Coding::Zstd => out = zstd::decode_all(encoded).unwrap(),
    }
    out
}
