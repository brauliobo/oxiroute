//! Server-side instruction cost per request, transport and TLS excluded: pre-encoded client frames are replayed
//! from memory into the server (this crate's, and the `h2` crate in two shapes for comparison) and the
//! user-space instructions of the process are counted with `perf stat -e instructions:u`. The cost per request is
//! the slope between two request counts, so everything that does not scale with the request count cancels.
//!
//! The workspace forbids `unsafe`, so the example does not call `perf_event_open` itself: with no arguments it
//! generates the client bytes and re-executes itself (`instr run ...`) under `flock /tmp/oxiroute-bench.lock perf
//! stat`.
//!
//! ```text
//! cargo build --release --example instr && target/release/examples/instr            # table
//! target/release/examples/instr profile ours                                        # perf record + report
//! ```
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::similar_names
)]
use std::future::{Future, poll_fn};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::{Pin, pin};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{self, HeaderName};
use http::{HeaderMap, HeaderValue, StatusCode};
use oxiroute_h2::hpack::{Encoder, Indexing};
use oxiroute_h2::server::{Config, Server};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const SIZES: [usize; 2] = [4000, 24000];
const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
const LOCK: &str = "/tmp/oxiroute-bench.lock";
const IMPLS: [(&str, &str); 3] = [
    ("ours", "oxiroute-h2"),
    ("h2-task", "h2 0.4.20, task per stream (as pingora does)"),
    ("h2-inline", "h2 0.4.20, handler inline"),
];

fn frame_header(out: &mut Vec<u8>, len: usize, kind: u8, flags: u8, stream: u32) {
    out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8, kind, flags]);
    out.extend_from_slice(&stream.to_be_bytes());
}

/// File layout: u64 count, then count + 1 end offsets (preamble, then each request), then the bytes.
fn client_bytes(n: usize) -> Vec<u8> {
    let mut out = PREFACE.to_vec();
    frame_header(&mut out, 0, 4, 0, 0);
    frame_header(&mut out, 0, 4, 1, 0);
    frame_header(&mut out, 4, 8, 0, 0);
    out.extend_from_slice(&(1u32 << 30).to_be_bytes());
    let mut ends = vec![out.len() as u64];
    let mut enc = Encoder::new(4096);
    for i in 0..n {
        let mut block = Vec::new();
        let unique = format!("{:016x}", i as u64 * 0x9e37_79b9_7f4a_7c15);
        let fields: [(&[u8], &[u8], Indexing); 7] = [
            (b":method", b"GET", Indexing::Incremental),
            (b":scheme", b"https", Indexing::Incremental),
            (b":path", b"/bench/item", Indexing::Incremental),
            (b":authority", b"example.com:8443", Indexing::Incremental),
            (
                b"user-agent",
                b"r64-bench/1.0 (compatible)",
                Indexing::Incremental,
            ),
            (b"accept", b"*/*", Indexing::Incremental),
            (b"x-trace", unique.as_bytes(), Indexing::None),
        ];
        for (name, value, indexing) in fields {
            enc.encode_field(&mut block, name, value, indexing);
        }
        frame_header(&mut out, block.len(), 1, 5, (2 * i + 1) as u32);
        out.extend_from_slice(&block);
        ends.push(out.len() as u64);
    }
    let mut file = Vec::new();
    file.extend_from_slice(&(n as u64).to_le_bytes());
    for e in &ends {
        file.extend_from_slice(&e.to_le_bytes());
    }
    file.extend_from_slice(&out);
    file
}

struct Replay {
    data: Vec<u8>,
    /// Chunk end offsets: the preamble, then every `per_read` requests.
    ends: Vec<usize>,
    pos: usize,
    chunk: usize,
    written: Arc<AtomicU64>,
}

impl Replay {
    fn load(path: &Path, per_read: usize, written: Arc<AtomicU64>) -> Self {
        let file = std::fs::read(path).expect("client bytes");
        let word =
            |i: usize| u64::from_le_bytes(file[8 * i..8 * i + 8].try_into().unwrap()) as usize;
        let n = word(0);
        let base = 8 * (n + 2);
        let ends_all: Vec<usize> = (0..=n).map(|i| word(1 + i)).collect();
        let mut ends = vec![ends_all[0]];
        ends.extend((per_read..=n).step_by(per_read).map(|r| ends_all[r]));
        if n % per_read != 0 {
            ends.push(ends_all[n]);
        }
        Self {
            data: file[base..].to_vec(),
            ends,
            pos: 0,
            chunk: 0,
            written,
        }
    }
}

impl AsyncRead for Replay {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let s = &mut *self;
        if s.chunk >= s.ends.len() {
            return Poll::Pending; // an open connection with nothing more to read
        }
        let end = s.ends[s.chunk];
        let n = (end - s.pos).min(buf.remaining());
        buf.put_slice(&s.data[s.pos..s.pos + n]);
        s.pos += n;
        if s.pos == end {
            s.chunk += 1;
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Replay {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.written.fetch_add(b.len() as u64, Ordering::Relaxed);
        Poll::Ready(Ok(b.len()))
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let n = bufs.iter().map(|b| b.len()).sum::<usize>();
        self.written.fetch_add(n as u64, Ordering::Relaxed);
        Poll::Ready(Ok(n))
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn run_ours(io: Replay, n: usize, written: &AtomicU64) -> u64 {
    let body = Bytes::from(vec![b'x'; 1024]);
    let mut server = Server::handshake(io, Config::default()).await.unwrap();
    let mut served = 0u64;
    while served < n as u64 {
        let (_parts, mut stream) = server.accept().await.unwrap().unwrap();
        let mut headers = HeaderMap::with_capacity(5);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1024"));
        headers.insert(header::SERVER, HeaderValue::from_static("oxiroute"));
        headers.insert(
            header::DATE,
            HeaderValue::from_static("Mon, 09 Nov 2026 12:00:00 GMT"),
        );
        headers.insert(
            REQUEST_ID,
            HeaderValue::from_str(&format!("{served:016x}")).unwrap(),
        );
        stream
            .send_response(StatusCode::OK, headers, false)
            .unwrap();
        let granted = poll_fn(|cx| stream.poll_capacity(cx, 1024)).await.unwrap();
        assert_eq!(granted, 1024);
        stream.send_data(body.clone(), true).unwrap();
        served += 1;
    }
    // One poll of `accept` runs the connection until everything queued has been written (the replay is idle).
    loop {
        let before = written.load(Ordering::Relaxed);
        {
            let mut accept = pin!(server.accept());
            poll_fn(|cx| {
                let _ = accept.as_mut().poll(cx);
                Poll::Ready(())
            })
            .await;
        }
        tokio::task::yield_now().await;
        if written.load(Ordering::Relaxed) == before {
            break;
        }
    }
    served
}

async fn run_h2(io: Replay, n: usize, spawn: bool, written: &AtomicU64) -> u64 {
    let body = Bytes::from(vec![b'x'; 1024]);
    let mut conn = h2::server::handshake(io).await.unwrap();
    let mut served = 0u64;
    let mut tasks = vec![];
    while let Some(r) = conn.accept().await {
        let (req, mut respond) = r.unwrap();
        let body = body.clone();
        let id = served;
        let work = async move {
            let resp = http::Response::builder()
                .status(200)
                .header("content-type", "application/octet-stream")
                .header("content-length", "1024")
                .header("server", "oxiroute")
                .header("date", "Mon, 09 Nov 2026 12:00:00 GMT")
                .header("x-request-id", format!("{id:016x}"))
                .body(())
                .unwrap();
            let _ = req;
            let mut send = respond.send_response(resp, false).unwrap();
            send.send_data(body, true).unwrap();
        };
        if spawn {
            tasks.push(tokio::spawn(work));
        } else {
            work.await;
        }
        served += 1;
        if served == n as u64 {
            break;
        }
    }
    for t in tasks {
        t.await.unwrap();
    }
    // One poll of `accept` runs the connection until everything queued has been written (the replay is idle).
    loop {
        let before = written.load(Ordering::Relaxed);
        {
            let mut accept = pin!(conn.accept());
            poll_fn(|cx| {
                let _ = accept.as_mut().poll(cx);
                Poll::Ready(())
            })
            .await;
        }
        tokio::task::yield_now().await;
        if written.load(Ordering::Relaxed) == before {
            break;
        }
    }
    served
}

/// `instr run <impl> <n> <file> <per_read>`: the measured process.
fn run(args: &[String]) {
    let (which, n, path, per_read) = (
        &args[0],
        args[1].parse().unwrap(),
        PathBuf::from(&args[2]),
        args[3].parse().unwrap(),
    );
    let written = Arc::new(AtomicU64::new(0));
    let io = Replay::load(&path, per_read, written.clone());
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let served = rt.block_on(async {
        match which.as_str() {
            "ours" => run_ours(io, n, &written).await,
            "h2-task" => run_h2(io, n, true, &written).await,
            _ => run_h2(io, n, false, &written).await,
        }
    });
    println!(
        "served={served} written={}",
        written.load(Ordering::Relaxed)
    );
    assert_eq!(served, n as u64);
}

fn temp_file(n: usize) -> PathBuf {
    std::env::temp_dir().join(format!("oxiroute-h2-instr-{}-{n}.bin", std::process::id()))
}

fn locked(cmd: &[&str]) -> Command {
    let mut c = Command::new("flock");
    c.arg(LOCK).args(cmd);
    c
}

fn instructions(which: &str, n: usize, per_read: usize) -> u64 {
    let exe = std::env::current_exe().unwrap();
    let out = locked(&["perf", "stat", "-e", "instructions:u", "-x,", "--"])
        .arg(exe)
        .args([
            "run",
            which,
            &n.to_string(),
            temp_file(n).to_str().unwrap(),
            &per_read.to_string(),
        ])
        .output()
        .expect("perf stat");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.contains("instructions:u"))
        .unwrap_or_else(|| panic!("no counter in {stderr}"));
    line.split(',')
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("counter not supported: {line}"))
}

fn profile(which: &str) {
    let n = SIZES[1];
    std::fs::write(temp_file(n), client_bytes(n)).unwrap();
    let data = std::env::temp_dir().join(format!("oxiroute-h2-instr-{}.perf", std::process::id()));
    let exe = std::env::current_exe().unwrap();
    let status = locked(&[
        "perf",
        "record",
        "-q",
        "-e",
        "instructions:u",
        "-c",
        "1000",
        "-o",
        data.to_str().unwrap(),
        "--",
    ])
    .arg(exe)
    .args([
        "run",
        which,
        &n.to_string(),
        temp_file(n).to_str().unwrap(),
        "16",
    ])
    .status()
    .expect("perf record");
    assert!(status.success());
    let report = Command::new("perf")
        .args([
            "report",
            "-i",
            data.to_str().unwrap(),
            "--stdio",
            "--no-children",
            "--sort",
            "symbol",
            "-F",
            "overhead,sample,symbol",
        ])
        .output()
        .expect("perf report");
    let text = String::from_utf8_lossy(&report.stdout);
    let mut rows: Vec<(f64, String)> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (pct, rest) = l.trim().split_once('%')?;
            let symbol = rest
                .split("  -")
                .next()?
                .split_once(" [.] ")?
                .1
                .trim()
                .to_string();
            Some((pct.trim().parse().ok()?, symbol))
        })
        .collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (pct, symbol) in rows.iter().take(30) {
        println!("{pct:>6.2}%  {symbol}");
    }
    let _ = std::fs::remove_file(data);
    let _ = std::fs::remove_file(temp_file(n));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => return run(&args[1..]),
        Some("profile") => return profile(args.get(1).map_or("ours", String::as_str)),
        _ => {}
    }
    let only = args.first().map(String::as_str);
    for n in SIZES {
        std::fs::write(temp_file(n), client_bytes(n)).unwrap();
    }
    for per_read in [1, 16] {
        println!("--- {per_read} request(s) per read ---");
        for (which, label) in IMPLS.iter().filter(|(w, _)| only.is_none_or(|o| o == *w)) {
            let counts = SIZES.map(|n| instructions(which, n, per_read));
            let slope = (counts[1] - counts[0]) as f64 / (SIZES[1] - SIZES[0]) as f64;
            println!("{label:<48} {slope:>9.0} instr/request");
            std::io::stdout().flush().unwrap();
        }
    }
    for n in SIZES {
        let _ = std::fs::remove_file(temp_file(n));
    }
}
