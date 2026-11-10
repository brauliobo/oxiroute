#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Static files over a plain HTTP/1.1 connection as seen on the wire. Large bodies leave the page cache for the
//! socket in the kernel (`sendfile`): whole files, ranges, `HEAD` and a keep-alive connection reused afterwards must
//! look exactly like the buffered path. The proxy is the real binary and the files live in memory (`/dev/shm`), the
//! kind of file system the kernel path is offered for without a non-blocking read.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, os::unix::fs::PermissionsExt as _};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use h1_wire::{Origin, STEP, Wire, find};

const BIG: usize = 3 * 1024 * 1024 + 17;

fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 7 + index / 251) % 251).expect("below 251"))
        .collect()
}

fn serve(root: std::path::PathBuf) -> impl FnOnce(&mut oxiroute_config::ConfigDraft) {
    move |config| {
        config.http_services[0].routes[0].action = serde_json::from_value(
            serde_json::json!({ "type": "static_files", "root_directory": root }),
        )
        .expect("static files action");
    }
}

async fn wire_serving(data: &[u8]) -> (Wire, tempfile::TempDir) {
    let directory = tempfile::tempdir_in("/dev/shm").expect("static root directory in memory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("create static root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure static root");
    fs::write(root.join("big.bin"), data).expect("write the large file");
    fs::write(root.join("small.txt"), b"small").expect("write the small file");
    let origin = Origin::start(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    (Wire::over(origin, serve(root)).await, directory)
}

/// One response: its head and its body, framed by `Content-Length`; a response to `HEAD` has no body.
async fn read_response(stream: &mut TcpStream, head_only: bool) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let head_end = loop {
        if let Some(position) = find(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response head")
            .expect("read");
        assert_ne!(count, 0, "closed before the head");
        bytes.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let length = if head_only {
        0
    } else {
        head.to_ascii_lowercase()
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .map(|value| value.trim().parse::<usize>().unwrap())
            })
            .expect("content length")
    };
    while bytes.len() < head_end + length {
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response body")
            .expect("read");
        assert_ne!(count, 0, "closed inside the body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(bytes.len(), head_end + length, "bytes beyond the response");
    (head, bytes[head_end..].to_vec())
}

async fn get(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    range: Option<&str>,
) -> (String, Vec<u8>) {
    let range = range
        .map(|value| format!("Range: bytes={value}\r\n"))
        .unwrap_or_default();
    let request = format!("{method} {path} HTTP/1.1\r\nHost: example.test\r\n{range}\r\n");
    stream.write_all(request.as_bytes()).await.expect("request");
    read_response(stream, method == "HEAD").await
}

#[tokio::test]
async fn large_files_arrive_intact_and_the_connection_serves_the_next_request() {
    let data = pattern(BIG);
    let (wire, _directory) = wire_serving(&data).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    for path in ["/big.bin", "/small.txt", "/big.bin", "/big.bin"] {
        let (head, body) = get(&mut client, "GET", path, None).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let expected: &[u8] = if path == "/big.bin" { &data } else { b"small" };
        assert!(body == expected, "{path}: body differs");
    }
    wire.finish();
}

#[tokio::test]
async fn ranges_of_a_large_file_are_exact() {
    let data = pattern(BIG);
    let (wire, _directory) = wire_serving(&data).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    let last = BIG - 1;
    for (start, end) in [
        (0, 99),
        (100, 1_500_000),
        (70_000, last),
        (BIG - 70_000, last),
        (12_345, 12_345),
    ] {
        let (head, body) = get(
            &mut client,
            "GET",
            "/big.bin",
            Some(&format!("{start}-{end}")),
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains(&format!("content-range: bytes {start}-{end}/{BIG}")),
            "{head}"
        );
        assert!(body == data[start..=end], "range {start}-{end} differs");
    }
    let (head, body) = get(&mut client, "GET", "/big.bin", Some("-1000")).await;
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert!(body == data[BIG - 1000..], "suffix range differs");
    wire.finish();
}

#[tokio::test]
async fn head_requests_for_a_large_file_have_no_body_and_keep_the_connection() {
    let data = pattern(BIG);
    let (wire, _directory) = wire_serving(&data).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    let (head, body) = get(&mut client, "HEAD", "/big.bin", None).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains(&format!("content-length: {BIG}")),
        "{head}"
    );
    assert!(body.is_empty());
    let (_, body) = get(&mut client, "GET", "/big.bin", None).await;
    assert!(body == data, "body after HEAD differs");
    wire.finish();
}

#[tokio::test]
async fn a_client_that_leaves_in_the_middle_of_a_large_file_does_not_stop_the_server() {
    let data = pattern(BIG * 4);
    let (wire, _directory) = wire_serving(&data).await;
    let mut dropped = TcpStream::connect(wire.proxy).await.expect("connect");
    dropped
        .write_all(b"GET /big.bin HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .await
        .expect("request");
    dropped
        .read_exact(&mut [0_u8; 1000])
        .await
        .expect("the beginning of the response");
    drop(dropped);
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    let (_, body) = get(&mut client, "GET", "/big.bin", None).await;
    assert!(body == data, "body after an abandoned download differs");
    wire.finish();
}

#[tokio::test]
async fn a_compressed_static_response_does_not_bypass_the_compression_module() {
    let data = pattern(BIG);
    let directory = tempfile::tempdir_in("/dev/shm").expect("static root directory in memory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("create static root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure static root");
    fs::write(root.join("big.bin"), &data).expect("write the large file");
    let origin = Origin::start(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    let wire = Wire::over(origin, |config| {
        serve(root)(config);
        config.http_services[0].gzip = Some(oxiroute_config::HttpGzipPolicy {
            content_types: vec!["application/octet-stream".into()],
            ..Default::default()
        });
    })
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client
        .write_all(b"GET /big.bin HTTP/1.1\r\nHost: example.test\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n")
        .await
        .expect("request");
    let mut bytes = Vec::new();
    timeout(STEP, client.read_to_end(&mut bytes))
        .await
        .expect("response")
        .expect("read");
    let head_end = find(&bytes, b"\r\n\r\n").expect("head") + 4;
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
    assert!(head.contains("content-encoding: gzip"), "{head}");
    let body = &bytes[head_end..];
    let first_payload = find(body, b"\r\n").expect("chunk size line") + 2;
    assert_eq!(
        body[first_payload..first_payload + 2],
        [0x1f, 0x8b],
        "not a gzip stream"
    );
    wire.finish();
}
