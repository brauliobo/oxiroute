//! User-space instructions per HPACK operation.
//!
//! The workspace forbids `unsafe`, so the counter is `perf stat -e instructions:u` on a child process running one
//! scenario at two iteration counts; the slope cancels process start-up and the one-time setup.
//!
//! `cargo run --release --example hpack_instr` prints the table; `hpack_instr run <scenario> <iters>` is the child.
use oxiroute_h2::hpack::{Decoder, Encoder, Indexing};
use std::hint::black_box;
use std::process::Command;

const REQUEST: [(&str, &str); 10] = [
    (":method", "GET"),
    (":scheme", "https"),
    (":path", "/static/app.js?v=20240611"),
    (":authority", "www.example.com"),
    (
        "user-agent",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    ),
    (
        "accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
    ),
    ("accept-language", "en-US,en;q=0.9"),
    ("accept-encoding", "gzip, deflate, br"),
    ("referer", "https://www.example.com/index.html"),
    (
        "cookie",
        "session=abc123def456ghi789; theme=dark; _ga=GA1.2.1234567890.1700000000",
    ),
];

type Field = (&'static str, &'static str, Indexing);

/// Five headers where only `content-length` is a literal on repeat.
const RESPONSE: [Field; 5] = [
    (
        "content-type",
        "text/html; charset=utf-8",
        Indexing::Incremental,
    ),
    ("content-length", "48213", Indexing::None),
    ("server", "oxiroute", Indexing::Incremental),
    (
        "cache-control",
        "public, max-age=3600",
        Indexing::Incremental,
    ),
    ("vary", "accept-encoding", Indexing::Incremental),
];

/// The same plus a `date` that differs on every response.
const RESPONSE_DATED: [Field; 6] = [
    (
        "content-type",
        "text/html; charset=utf-8",
        Indexing::Incremental,
    ),
    ("content-length", "48213", Indexing::None),
    ("date", "Tue, 11 Jun 2024 10:15:30 GMT", Indexing::None),
    ("server", "oxiroute", Indexing::Incremental),
    (
        "cache-control",
        "public, max-age=3600",
        Indexing::Incremental,
    ),
    ("vary", "accept-encoding", Indexing::Incremental),
];

fn request_block(enc: &mut Encoder) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, v) in REQUEST {
        enc.encode_field(&mut out, n.as_bytes(), v.as_bytes(), Indexing::Incremental);
    }
    out
}

fn decode(dec: &mut Decoder, block: &[u8]) {
    let mut sum = 0;
    dec.decode(block, 1 << 16, |n, v| sum += n.len() + v.len())
        .unwrap();
    black_box(sum);
}

fn encode_response(enc: &mut Encoder, out: &mut Vec<u8>, fields: &[(&str, &str, Indexing)]) {
    out.clear();
    for &(n, v, i) in fields {
        enc.encode_field(out, n.as_bytes(), v.as_bytes(), i);
    }
    black_box(&out);
}

fn run(scenario: &str, iters: u64) {
    let mut enc = Encoder::new(4096);
    let first = request_block(&mut enc);
    let repeat = request_block(&mut enc);
    let mut dec = Decoder::new(4096);
    decode(&mut dec, &first);
    let mut out = Vec::new();
    let (mut resp_enc, mut dated_enc) = (Encoder::new(4096), Encoder::new(4096));
    encode_response(&mut resp_enc, &mut out, &RESPONSE);
    encode_response(&mut dated_enc, &mut out, &RESPONSE_DATED);
    for _ in 0..iters {
        match scenario {
            "decode_first" => decode(&mut Decoder::new(4096), black_box(&first)),
            "decode_repeat" => decode(&mut dec, black_box(&repeat)),
            "encode_first" => encode_response(&mut Encoder::new(4096), &mut out, &RESPONSE),
            "encode_repeat" => encode_response(&mut resp_enc, &mut out, &RESPONSE),
            "encode_repeat_dated" => encode_response(&mut dated_enc, &mut out, &RESPONSE_DATED),
            other => panic!("unknown scenario {other}"),
        }
    }
}

fn instructions(scenario: &str, iters: u64) -> u64 {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new("perf")
        .args(["stat", "-x,", "-e", "instructions:u", "--"])
        .arg(exe)
        .args(["run", scenario, &iters.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    let line = text
        .lines()
        .find(|l| l.contains("instructions"))
        .expect("perf output");
    line.split(',').next().unwrap().parse().unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("run") {
        return run(&args[2], args[3].parse().unwrap());
    }
    let (lo, hi) = (2_000, 12_000);
    for scenario in [
        "decode_first",
        "decode_repeat",
        "encode_first",
        "encode_repeat",
        "encode_repeat_dated",
    ] {
        let per_op = (instructions(scenario, hi) - instructions(scenario, lo)) / (hi - lo);
        println!("{scenario:<20} {per_op:>6} instructions");
    }
}
