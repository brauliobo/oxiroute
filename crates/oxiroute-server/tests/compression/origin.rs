#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

use std::{
    fmt::Write as _,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use oxiroute_compress::{Coding, Levels, compress};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

pub const BODY_BYTES: usize = 8192;

pub fn html() -> Vec<u8> {
    let mut page = String::new();
    let mut row = 0_u32;
    while page.len() < BODY_BYTES {
        let _ = writeln!(
            page,
            "<tr class=\"row\"><td>{row}</td><td>item-{}</td></tr>",
            row * 7 % 13
        );
        row += 1;
    }
    page.truncate(BODY_BYTES);
    page.into_bytes()
}

pub fn noise(seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..BODY_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[3]
        })
        .collect()
}

pub fn hex_text(seed: u64) -> Vec<u8> {
    noise(seed)
        .into_iter()
        .map(|byte| b"0123456789abcdef"[usize::from(byte & 15)])
        .collect()
}

struct Page {
    content_type: &'static str,
    extra: &'static str,
    body: Vec<u8>,
}

fn page(path: &str) -> Option<Page> {
    let (content_type, extra, body) = match path {
        "/page.html" => ("text/html; charset=utf-8", "ETag: \"v1\"\r\n", html()),
        "/weak.html" => ("text/html", "ETag: W/\"w1\"\r\n", html()),
        "/noetag.html" => ("text/html", "", html()),
        "/tiny.html" => ("text/html", "ETag: \"tiny\"\r\n", html()[..100].to_vec()),
        "/image.png" => ("image/png", "", noise(7)),
        "/random.html" => ("text/html", "", noise(11)),
        "/pre.html" => (
            "text/html",
            "Content-Encoding: gzip\r\n",
            compress(Coding::Gzip, Levels::default(), &html()).expect("gzip"),
        ),
        "/notransform.html" => (
            "text/html",
            "Cache-Control: no-transform, max-age=3600\r\n",
            html(),
        ),
        "/a.txt" => ("text/plain", "", hex_text(1)),
        "/b.txt" => ("text/plain", "", hex_text(2)),
        "/c.txt" => ("text/plain", "", hex_text(3)),
        _ => return None,
    };
    Some(Page {
        content_type,
        extra,
        body,
    })
}

fn matches_condition(head: &str, extra: &str) -> bool {
    let weak = |tag: &str| tag.trim().trim_start_matches("W/").to_owned();
    let condition = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("if-none-match")
            .then(|| weak(value))
    });
    let current = extra
        .lines()
        .find_map(|line| line.strip_prefix("ETag:").map(weak));
    condition.is_some() && condition == current
}

fn reply(path: &str, head: &str) -> Vec<u8> {
    let Some(page) = page(path) else {
        return b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec();
    };
    if matches_condition(head, page.extra) {
        return format!("HTTP/1.1 304 Not Modified\r\n{}\r\n", page.extra).into_bytes();
    }
    let cache_control = if page.extra.contains("Cache-Control") {
        ""
    } else {
        "Cache-Control: public, max-age=3600\r\n"
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\n{}{cache_control}Content-Length: {}\r\n\r\n",
        page.content_type,
        page.extra,
        page.body.len()
    );
    [head.into_bytes(), page.body].concat()
}

pub struct CompressionOrigin {
    pub address: SocketAddr,
    paths: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl CompressionOrigin {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let paths = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&paths);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                tokio::spawn(serve(stream, Arc::clone(&seen)));
            }
        });
        Self {
            address,
            paths,
            task,
        }
    }

    pub fn requests(&self, path: &str) -> usize {
        let paths = self.paths.lock().expect("paths");
        paths.iter().filter(|seen| *seen == path).count()
    }

    pub fn finish(self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, paths: Arc<Mutex<Vec<String>>>) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        while let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end + 4]).into_owned();
            bytes.drain(..end + 4);
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            paths.lock().expect("paths").push(path.clone());
            if stream.write_all(&reply(&path, &head)).await.is_err() {
                return;
            }
        }
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
        }
    }
}
