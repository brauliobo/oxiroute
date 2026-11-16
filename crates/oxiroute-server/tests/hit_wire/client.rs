//! A keep-alive HTTP/1.1 client that reads one response at a time, framed by `Content-Length`.

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use crate::h1_wire::{STEP, find};

pub struct Reply {
    pub head: String,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn status(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }

    /// The value of the first field named `name`, whatever its case.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.head.split("\r\n").skip(1).find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}

pub struct Client {
    stream: TcpStream,
}

impl Client {
    pub async fn connect(address: std::net::SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(address).await.expect("connect"),
        }
    }

    pub async fn get(&mut self, path: &str, fields: &str) -> Reply {
        self.request("GET", path, fields).await
    }

    pub async fn request(&mut self, method: &str, path: &str, fields: &str) -> Reply {
        let request = format!("{method} {path} HTTP/1.1\r\nHost: cache.test\r\n{fields}\r\n");
        self.send(request.as_bytes(), method == "HEAD").await
    }

    pub async fn send(&mut self, request: &[u8], head_only: bool) -> Reply {
        self.stream.write_all(request).await.expect("request");
        self.read(head_only).await
    }

    /// Whether the proxy closed the connection: nothing more arrives, and the end of the stream does.
    pub async fn closed(&mut self) -> bool {
        let mut buffer = [0_u8; 64];
        matches!(
            timeout(STEP, self.stream.read(&mut buffer)).await,
            Ok(Ok(0) | Err(_))
        )
    }

    async fn read(&mut self, head_only: bool) -> Reply {
        let mut bytes = Vec::new();
        let mut buffer = vec![0_u8; 256 * 1024];
        let head_end = loop {
            if let Some(position) = find(&bytes, b"\r\n\r\n") {
                break position + 4;
            }
            let count = timeout(STEP, self.stream.read(&mut buffer))
                .await
                .expect("response head")
                .expect("read");
            assert_ne!(count, 0, "closed before the head: {bytes:?}");
            bytes.extend_from_slice(&buffer[..count]);
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
        let status = head.lines().next().unwrap_or_default();
        let bodiless = head_only || status.contains(" 204 ") || status.contains(" 304 ");
        let length = if bodiless {
            0
        } else {
            head.to_ascii_lowercase()
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0)
        };
        while bytes.len() < head_end + length {
            let count = timeout(STEP, self.stream.read(&mut buffer))
                .await
                .expect("response body")
                .expect("read");
            assert_ne!(count, 0, "closed inside the body of {head}");
            bytes.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(bytes.len(), head_end + length, "bytes beyond {head}");
        Reply {
            head,
            body: bytes[head_end..].to_vec(),
        }
    }
}
