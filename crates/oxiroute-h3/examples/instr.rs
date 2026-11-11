//! Server-thread instruction cost per HTTP/3 request over loopback QUIC: the `h3` crate server as shipped, this
//! crate with a task per request, and this crate handling requests inline in the connection task.
//!
//! The server is its own process with a current-thread runtime, run under `perf stat -e instructions:u`, so the
//! counter holds the server thread and nothing else; the `h3` clients run in another process. The workspace forbids
//! `unsafe`, so the example does not call `perf_event_open` itself. 8 connections x 4 concurrent streams, 1 KiB
//! responses. The cost per request is the slope between two request counts (handshakes and setup cancel); the total
//! over the larger count is printed too.
//!
//! ```text
//! cargo build --release --example instr
//! target/release/examples/instr            # all lanes, 3 runs each
//! target/release/examples/instr h3         # h3 | thin-task | thin-inline | prep-task | prep-inline | head
//! target/release/examples/instr profile thin-task   # hottest symbols of the server thread
//! ```
//!
//! Every measured run holds `flock /tmp/oxiroute-bench.lock`.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::similar_names
)]
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::Arc;

use bytes::{Buf, Bytes};
use http::{HeaderValue, Response, header};
use oxiroute_h3::server::{Config, Connection, PreparedHead, RequestStream};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const LOCK: &str = "/tmp/oxiroute-bench.lock";
const CONNECTIONS: usize = 8;
const STREAMS: usize = 4;
const SIZES: [usize; 2] = [8000, 16000];
const HEAD_SIZES: [usize; 2] = [100_000, 200_000];
const RUNS: usize = 3;
const LANES: [(&str, &str); 5] = [
    ("h3", "h3 crate server (as shipped), task per request"),
    ("thin-task", "oxiroute-h3, task per request"),
    ("thin-inline", "oxiroute-h3, requests handled inline"),
    ("prep-task", "oxiroute-h3, prepared head, task per request"),
    ("prep-inline", "oxiroute-h3, prepared head, inline"),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn response() -> Response<()> {
    let mut response = Response::new(());
    let headers = response.headers_mut();
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
    response
}

async fn thin_request(mut stream: RequestStream, body: Bytes, head: Option<PreparedHead>) {
    let request = stream.read_head().await.unwrap();
    std::hint::black_box(&request);
    match head {
        Some(head) => stream.send_prepared(&head, body, false).await.unwrap(),
        None => stream
            .send_full_response(response(), body, false)
            .await
            .unwrap(),
    }
}

async fn serve_thin(conn: quinn::Connection, body: Bytes, inline: bool, prepared: bool) -> u64 {
    let mut connection = Connection::new(conn, Config::default()).await.unwrap();
    let head = prepared.then(|| PreparedHead::new(&response()));
    let mut served = 0;
    loop {
        let stream = match connection.accept().await {
            Ok(Some(stream)) => stream,
            Ok(None) => break,
            Err(error) => {
                eprintln!("accept failed: {error}");
                break;
            }
        };
        served += 1;
        if inline {
            thin_request(stream, body.clone(), head.clone()).await;
        } else {
            tokio::spawn(thin_request(stream, body.clone(), head.clone()));
        }
    }
    served
}

async fn serve_h3(conn: quinn::Connection, body: Bytes) -> u64 {
    let mut h3: h3::server::Connection<_, Bytes> =
        h3::server::Connection::new(h3_quinn::Connection::new(conn))
            .await
            .unwrap();
    let mut served = 0;
    while let Ok(Some(resolver)) = h3.accept().await {
        served += 1;
        let body = body.clone();
        tokio::spawn(async move {
            let (_request, mut stream) = resolver.resolve_request().await.unwrap();
            stream.send_response(response()).await.unwrap();
            stream.send_data(body).await.unwrap();
            stream.finish().await.unwrap();
        });
    }
    served
}

/// `instr serve <lane> <connections> <cert hex> <key hex>`: prints its address, serves, prints `served=N`.
fn serve(args: &[String]) {
    let lane = args[0].as_str();
    let connections: usize = args[1].parse().unwrap();
    let cert = CertificateDer::from(unhex(&args[2]));
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(unhex(&args[3])));
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(256u32.into());
    transport
        .stream_receive_window((1u32 << 20).into())
        .receive_window((16u32 << 20).into());
    config.transport_config(Arc::new(transport));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let served = runtime.block_on(async move {
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        println!("{}", endpoint.local_addr().unwrap());
        std::io::stdout().flush().unwrap();
        let body = Bytes::from(vec![b'x'; 1024]);
        let mut tasks = Vec::new();
        for _ in 0..connections {
            let conn = endpoint.accept().await.unwrap().await.unwrap();
            let body = body.clone();
            tasks.push(match lane {
                "h3" => tokio::spawn(serve_h3(conn, body)),
                "thin-task" => tokio::spawn(serve_thin(conn, body, false, false)),
                "thin-inline" => tokio::spawn(serve_thin(conn, body, true, false)),
                "prep-task" => tokio::spawn(serve_thin(conn, body, false, true)),
                _ => tokio::spawn(serve_thin(conn, body, true, true)),
            });
        }
        let mut served = 0;
        for task in tasks {
            served += task.await.unwrap();
        }
        served
    });
    println!("served={served}");
}

async fn client(addr: SocketAddr, cert: CertificateDer<'static>, total: usize) {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls).unwrap()));
    let mut transport = quinn::TransportConfig::default();
    transport
        .stream_receive_window((1u32 << 20).into())
        .receive_window((16u32 << 20).into());
    config.transport_config(Arc::new(transport));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(config);
    let per_connection = total / CONNECTIONS;
    let mut tasks = Vec::new();
    for _ in 0..CONNECTIONS {
        let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        tasks.push(tokio::spawn(async move {
            let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(conn.clone()))
                .await
                .unwrap();
            tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });
            let mut workers = Vec::new();
            for _ in 0..STREAMS {
                let mut send = send.clone();
                workers.push(tokio::spawn(async move {
                    for _ in 0..per_connection / STREAMS {
                        let request = http::Request::get("https://localhost/bench/item")
                            .body(())
                            .unwrap();
                        let mut stream = send.send_request(request).await.unwrap();
                        stream.finish().await.unwrap();
                        stream.recv_response().await.unwrap();
                        let mut got = 0;
                        while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                            got += chunk.remaining();
                            chunk.advance(chunk.remaining());
                        }
                        assert_eq!(got, 1024);
                    }
                }));
            }
            for worker in workers {
                worker.await.unwrap();
            }
            conn.close(0u32.into(), b"");
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    endpoint.wait_idle().await;
}

fn parse_instructions(perf: &str) -> u64 {
    let line = perf
        .lines()
        .find(|l| l.contains("instructions:u"))
        .unwrap_or_else(|| panic!("no counter in {perf}"));
    line.split(',')
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("counter not supported: {line}"))
}

/// `instr run <lane> <requests> [profile]`: one measured run; prints `instructions=N served=M`, or with `profile`
/// the hottest symbols of the server thread instead.
fn run(args: &[String]) {
    let (lane, total): (&str, usize) = (&args[0], args[1].parse().unwrap());
    let profile = args.get(2).is_some_and(|mode| mode == "profile");
    let data = std::env::temp_dir().join(format!("oxiroute-h3-instr-{}.perf", std::process::id()));
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = certified.cert.der().clone();
    let mut perf = Command::new("perf");
    if profile {
        perf.args([
            "record",
            "-q",
            "-e",
            "instructions:u",
            "-c",
            "1000",
            "-o",
            data.to_str().unwrap(),
            "--",
        ]);
    } else {
        perf.args(["stat", "-e", "instructions:u", "-x,", "--"]);
    }
    let mut server = perf
        .arg(std::env::current_exe().unwrap())
        .args([
            "serve",
            lane,
            &CONNECTIONS.to_string(),
            &hex(&cert),
            &hex(&certified.signing_key.serialize_der()),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("perf");
    let mut stdout = BufReader::new(server.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let addr: SocketAddr = line
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("server said {line:?}"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(client(addr, cert, total));
    }));
    if let Err(panic) = outcome {
        let _ = server.kill();
        let mut diagnostics = String::new();
        let _ = server
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostics);
        eprintln!("client failed; server output:\n{diagnostics}");
        std::panic::resume_unwind(panic);
    }
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let mut stderr = String::new();
    server
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(server.wait().unwrap().success(), "{stderr}");
    let served: usize = rest
        .trim()
        .strip_prefix("served=")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(served, total);
    if profile {
        print_profile(&data);
    } else {
        println!(
            "instructions={} served={served}",
            parse_instructions(&stderr)
        );
    }
}

fn print_profile(data: &std::path::Path) {
    let report = Command::new("perf")
        .args([
            "report",
            "-i",
            data.to_str().unwrap(),
            "--stdio",
            "--no-children",
            "--sort",
            "symbol",
        ])
        .output()
        .expect("perf report");
    let text = String::from_utf8_lossy(&report.stdout);
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .take(45)
    {
        println!("{line}");
    }
    let _ = std::fs::remove_file(data);
}

/// `instr head-run <iterations>`: decodes a typical request section that many times.
fn head_run(args: &[String]) {
    let iterations: usize = args[0].parse().unwrap();
    let mut block = vec![0, 0, 0xd1, 0xd7, 0x50, 9];
    block.extend_from_slice(b"localhost");
    block.push(0x51);
    block.push(15);
    block.extend_from_slice(b"/bench/item?a=1");
    for (name, value) in [
        ("user-agent", "bench/1.0 (compatible)"),
        ("accept", "*/*"),
        ("x-trace", "0123456789abcdef"),
    ] {
        block.push(0x20 | name.len().min(7) as u8);
        if name.len() >= 7 {
            block.push((name.len() - 7) as u8);
        }
        block.extend_from_slice(name.as_bytes());
        block.push(value.len() as u8);
        block.extend_from_slice(value.as_bytes());
    }
    for _ in 0..iterations {
        let request = oxiroute_h3::head::request(std::hint::black_box(&block), 16 * 1024).unwrap();
        std::hint::black_box(request);
    }
}

fn locked(args: &[&str]) -> Command {
    let mut command = Command::new("flock");
    command
        .arg(LOCK)
        .arg(std::env::current_exe().unwrap())
        .args(args);
    command
}

fn measured(args: &[&str]) -> (u64, Option<usize>) {
    let out = locked(args).output().expect("flock");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("instructions="))
        .unwrap_or_else(|| panic!("no result in {text}"));
    let mut fields = line.split(' ');
    let instructions = fields.next().unwrap()["instructions=".len()..]
        .parse()
        .unwrap();
    (
        instructions,
        fields.next().map(|s| s["served=".len()..].parse().unwrap()),
    )
}

/// `instr head-count <iterations>`: the decode loop under `perf stat`.
fn head_count(args: &[String]) {
    let out = Command::new("perf")
        .args(["stat", "-e", "instructions:u", "-x,", "--"])
        .arg(std::env::current_exe().unwrap())
        .args(["head-run", &args[0]])
        .output()
        .expect("perf stat");
    println!(
        "instructions={}",
        parse_instructions(&String::from_utf8_lossy(&out.stderr))
    );
}

fn profile(lane: &str) {
    let out = locked(&["run", lane, "16000", "profile"])
        .output()
        .expect("flock");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    print!("{}", String::from_utf8_lossy(&out.stdout));
}

fn report_head() {
    let counts = HEAD_SIZES.map(|n| measured(&["head-count", &n.to_string()]).0);
    let slope = (counts[1] - counts[0]) as f64 / (HEAD_SIZES[1] - HEAD_SIZES[0]) as f64;
    println!(
        "{:<48} {slope:>9.0} instr/request (decode and build the http::Request, 7 fields)",
        "head"
    );
}

fn report_lane(lane: &str, label: &str) {
    let mut line = String::new();
    for run in 0..RUNS {
        let counts = SIZES.map(|n| measured(&["run", lane, &n.to_string()]).0);
        let slope = (counts[1] - counts[0]) as f64 / (SIZES[1] - SIZES[0]) as f64;
        let _ = write!(line, " {slope:>8.0}");
        eprintln!("  run {run}: total {} for {} requests", counts[1], SIZES[1]);
    }
    println!("{label:<48}{line}   (instr/request, slope, {RUNS} runs)");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => return serve(&args[1..]),
        Some("run") => return run(&args[1..]),
        Some("head-run") => return head_run(&args[1..]),
        Some("head-count") => return head_count(&args[1..]),
        Some("profile") => return profile(args.get(1).map_or("thin-task", String::as_str)),
        _ => {}
    }
    let only = args.first().map(String::as_str);
    for (lane, label) in LANES
        .iter()
        .filter(|(lane, _)| only.is_none_or(|o| o == *lane))
    {
        report_lane(lane, label);
        std::io::stdout().flush().unwrap();
    }
    if only.is_none_or(|o| o == "head") {
        report_head();
    }
}
