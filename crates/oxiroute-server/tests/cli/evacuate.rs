//! The requests `oxiroute server evacuate` and `oxiroute pool evacuate` send, against a recording endpoint.

use std::{
    fs,
    io::{Read as _, Write as _},
    net::TcpListener,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Output},
    thread::{self, JoinHandle},
};

use serde_json::{Value, json};
use tempfile::TempDir;

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const REVISION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REPORT: &str = r#"{"outcome":"evacuated","force":false,"cancelledAndRedispatched":2,"forced":0,"leftRunning":{"response_started":1}}"#;

struct Exchange {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Value,
}

/// Answers the revision lookup, then the mutation, and returns what the CLI sent.
fn recording_endpoint(
    mutation_status: u16,
    mutation_body: &'static str,
) -> (String, JoinHandle<Vec<Exchange>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("endpoint");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let recorder = thread::spawn(move || {
        let revision = json!({ "generation": { "activeRevision": REVISION } }).to_string();
        [(200, revision), (mutation_status, mutation_body.to_owned())]
            .into_iter()
            .map(|(status, response)| {
                let (mut stream, _) = listener.accept().expect("request");
                let exchange = read_exchange(&mut stream);
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .expect("response");
                exchange
            })
            .collect()
    });
    (endpoint, recorder)
}

fn read_exchange(stream: &mut std::net::TcpStream) -> Exchange {
    let mut received = Vec::new();
    let mut byte = [0_u8; 1];
    while !received.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("request head");
        received.push(byte[0]);
    }
    let head = String::from_utf8(received).expect("UTF-8 head");
    let header = |name: &str| {
        head.lines().find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with(name)
                .then(|| line[name.len()..].trim().to_owned())
        })
    };
    let length = header("content-length:").map_or(0, |value| value.parse().expect("length"));
    let mut body = vec![0; length];
    stream.read_exact(&mut body).expect("request body");
    let mut request_line = head.lines().next().expect("request line").split(' ');
    Exchange {
        method: request_line.next().expect("method").to_owned(),
        path: request_line.next().expect("path").to_owned(),
        authorization: header("authorization:"),
        body: if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).expect("JSON body")
        },
    }
}

fn token_file(directory: &TempDir) -> String {
    let path = directory.path().join("management.token");
    fs::write(&path, TOKEN).expect("token");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    path.display().to_string()
}

fn run(endpoint: &str, token: &str, words: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .args(["--endpoint", endpoint, "--token-file", token])
        .args(words)
        .output()
        .expect("CLI")
}

fn assert_mutation(sent: &[Exchange], path: &str, mut body: Value) {
    assert_eq!(
        (sent[0].method.as_str(), sent[0].path.as_str()),
        ("GET", "/api/v1/generations")
    );
    assert_eq!(
        (sent[1].method.as_str(), sent[1].path.as_str()),
        ("POST", path)
    );
    assert_eq!(
        sent[1].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    body["expectedActiveRevision"] = json!(REVISION);
    assert_eq!(sent[1].body, body);
}

#[test]
fn server_evacuate_posts_every_pool_and_the_force_flag_and_prints_the_report() {
    let directory = TempDir::new().expect("directory");
    let (endpoint, recorder) = recording_endpoint(200, REPORT);

    let output = run(
        &endpoint,
        &token_file(&directory),
        &[
            "--output",
            "json",
            "server",
            "evacuate",
            "--force",
            "--pool",
            "gpu-render",
            "--pool",
            "voice-tts",
            "node-a",
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8"),
        format!("{REPORT}\n")
    );
    assert_mutation(
        &recorder.join().expect("recorder"),
        "/api/v1/servers/evacuate",
        json!({
            "targets": [
                { "pool": "gpu-render", "server": "node-a" },
                { "pool": "voice-tts", "server": "node-a" },
            ],
            "force": true,
        }),
    );
}

#[test]
fn pool_evacuate_posts_the_pools_without_force_by_default() {
    let directory = TempDir::new().expect("directory");
    let (endpoint, recorder) = recording_endpoint(200, REPORT);

    let output = run(
        &endpoint,
        &token_file(&directory),
        &["--quiet", "pool", "evacuate", "gpu-render", "voice-tts"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "--quiet prints nothing");
    assert_mutation(
        &recorder.join().expect("recorder"),
        "/api/v1/pools/evacuate",
        json!({ "pools": ["gpu-render", "voice-tts"], "force": false }),
    );
}

#[test]
fn an_unknown_pool_is_a_failed_command() {
    let directory = TempDir::new().expect("directory");
    let (endpoint, recorder) = recording_endpoint(404, r#"{"error":{"code":"pool_not_found"}}"#);

    let output = run(
        &endpoint,
        &token_file(&directory),
        &["pool", "evacuate", "missing"],
    );

    assert!(!output.status.success());
    recorder.join().expect("recorder");
}
