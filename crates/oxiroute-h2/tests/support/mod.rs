//! Test helpers: a raw HTTP/2 wire (frame writer and reader over an in-memory pipe), request header block
//! encoding, and the servers under test (this crate's and the `h2` crate's) with the same inline responder.
#![allow(dead_code)]
use std::future::poll_fn;
use std::task::Poll;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use http::{HeaderMap, StatusCode};
use oxiroute_h2::hpack::{Decoder, Encoder, Indexing};
use oxiroute_h2::server::{Config, Server};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

pub const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
pub const END_STREAM: u8 = 1;
pub const END_HEADERS: u8 = 4;
pub const PADDED: u8 = 8;
pub const PRIORITY_FLAG: u8 = 0x20;

pub const DATA: u8 = 0;
pub const HEADERS: u8 = 1;
pub const RST_STREAM: u8 = 3;
pub const SETTINGS: u8 = 4;
pub const PING: u8 = 6;
pub const GOAWAY: u8 = 7;
pub const WINDOW_UPDATE: u8 = 8;

pub fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let n = payload.len() as u32;
    let mut v = vec![(n >> 16) as u8, (n >> 8) as u8, n as u8, kind, flags];
    v.extend_from_slice(&stream.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn settings(entries: &[(u16, u32)]) -> Vec<u8> {
    let p: Vec<u8> = entries
        .iter()
        .flat_map(|(i, v)| [i.to_be_bytes().as_slice(), &v.to_be_bytes()].concat())
        .collect();
    frame(SETTINGS, 0, 0, &p)
}

pub fn settings_ack() -> Vec<u8> {
    frame(SETTINGS, 1, 0, &[])
}

pub fn headers(stream: u32, block: &[u8], flags: u8) -> Vec<u8> {
    frame(HEADERS, flags, stream, block)
}

pub fn continuation(stream: u32, block: &[u8], flags: u8) -> Vec<u8> {
    frame(9, flags, stream, block)
}

pub fn data(stream: u32, payload: &[u8], flags: u8) -> Vec<u8> {
    frame(DATA, flags, stream, payload)
}

pub fn rst(stream: u32, code: u32) -> Vec<u8> {
    frame(RST_STREAM, 0, stream, &code.to_be_bytes())
}

pub fn ping(payload: [u8; 8], ack: bool) -> Vec<u8> {
    frame(PING, u8::from(ack), 0, &payload)
}

pub fn window_update(stream: u32, inc: u32) -> Vec<u8> {
    frame(WINDOW_UPDATE, 0, stream, &inc.to_be_bytes())
}

pub fn priority(stream: u32, dep: u32) -> Vec<u8> {
    let mut p = dep.to_be_bytes().to_vec();
    p.push(15);
    frame(2, 0, stream, &p)
}

pub fn goaway(last: u32, code: u32) -> Vec<u8> {
    frame(
        GOAWAY,
        0,
        0,
        &[last.to_be_bytes(), code.to_be_bytes()].concat(),
    )
}

/// A client-side HPACK encoder for request header blocks.
pub struct Client {
    enc: Encoder,
}

impl Client {
    pub fn new() -> Self {
        Self {
            enc: Encoder::new(4096),
        }
    }

    pub fn block(&mut self, fields: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, v) in fields {
            self.enc
                .encode_field(&mut out, n.as_bytes(), v.as_bytes(), Indexing::Incremental);
        }
        out
    }

    pub fn get_block(&mut self, path: &str) -> Vec<u8> {
        self.block(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":authority", "example.test"),
            (":path", path),
        ])
    }

    pub fn post_block(&mut self, path: &str) -> Vec<u8> {
        self.block(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":authority", "example.test"),
            (":path", path),
        ])
    }

    /// A complete GET request on `stream` (END_STREAM, END_HEADERS).
    pub fn get(&mut self, stream: u32, path: &str) -> Vec<u8> {
        let b = self.get_block(path);
        headers(stream, &b, END_STREAM | END_HEADERS)
    }

    /// A POST whose body follows in DATA frames.
    pub fn post(&mut self, stream: u32, path: &str) -> Vec<u8> {
        let b = self.post_block(path);
        headers(stream, &b, END_HEADERS)
    }
}

#[derive(Clone, Debug)]
pub struct Raw {
    pub kind: u8,
    pub flags: u8,
    pub stream: u32,
    pub payload: Vec<u8>,
}

/// What a frame from the server means for a differential comparison.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reaction {
    GoAway(u32),
    Reset(u32, u32),
    Status(u32, u16),
}

/// The client end of an in-memory connection.
pub struct Wire {
    pub io: DuplexStream,
    buf: BytesMut,
    dec: Decoder,
    pub frames: Vec<Raw>,
}

impl Wire {
    pub fn new(io: DuplexStream) -> Self {
        Self {
            io,
            buf: BytesMut::new(),
            dec: Decoder::new(4096),
            frames: Vec::new(),
        }
    }

    pub async fn send(&mut self, bytes: &[u8]) {
        self.io.write_all(bytes).await.expect("server alive");
        self.io.flush().await.unwrap();
    }

    /// Next frame, `None` at EOF.
    pub async fn read_frame(&mut self) -> Option<Raw> {
        loop {
            if self.buf.len() >= 9 {
                let len = usize::from(self.buf[0]) << 16
                    | usize::from(self.buf[1]) << 8
                    | usize::from(self.buf[2]);
                if self.buf.len() >= 9 + len {
                    let h = self.buf.split_to(9);
                    let payload = self.buf.split_to(len).to_vec();
                    let stream = u32::from_be_bytes([h[5] & 0x7f, h[6], h[7], h[8]]);
                    let f = Raw {
                        kind: h[3],
                        flags: h[4],
                        stream,
                        payload,
                    };
                    self.frames.push(f.clone());
                    return Some(f);
                }
            }
            if self.io.read_buf(&mut self.buf).await.unwrap_or(0) == 0 {
                return None;
            }
        }
    }

    /// Reads until the server has been silent for a second of virtual time (tests run with paused time, so
    /// this means: everything runnable has run) or the connection ended.
    pub async fn settle(&mut self) -> Vec<Raw> {
        let start = self.frames.len();
        while let Ok(Some(_)) =
            tokio::time::timeout(Duration::from_secs(1), self.read_frame()).await
        {}
        self.frames[start..].to_vec()
    }

    pub async fn read_until(&mut self, mut stop: impl FnMut(&Raw) -> bool) -> Option<Raw> {
        loop {
            let f = self.read_frame().await?;
            if stop(&f) {
                return Some(f);
            }
        }
    }

    /// Sends the preface and an empty SETTINGS (plus the ACK of the server's), ready for requests.
    pub async fn start(&mut self) {
        self.send(&[PREFACE, &settings(&[]), &settings_ack()].concat())
            .await;
    }

    pub fn reactions(&mut self, frames: &[Raw]) -> Vec<Reaction> {
        let mut out = Vec::new();
        for f in frames {
            match f.kind {
                GOAWAY => out.push(Reaction::GoAway(u32::from_be_bytes(
                    f.payload[4..8].try_into().unwrap(),
                ))),
                RST_STREAM => out.push(Reaction::Reset(
                    f.stream,
                    u32::from_be_bytes(f.payload[..4].try_into().unwrap()),
                )),
                HEADERS => {
                    let mut status = 0;
                    self.dec
                        .decode(&f.payload, usize::MAX, |n, v| {
                            if n == b":status" {
                                status = std::str::from_utf8(v).unwrap().parse().unwrap();
                            }
                        })
                        .unwrap();
                    out.push(Reaction::Status(f.stream, status));
                }
                _ => {}
            }
        }
        out.sort();
        out
    }
}

pub fn goaway_code(f: &Raw) -> u32 {
    u32::from_be_bytes(f.payload[4..8].try_into().unwrap())
}

pub fn rst_code(f: &Raw) -> u32 {
    u32::from_be_bytes(f.payload[..4].try_into().unwrap())
}

/// Responds to every request inline: 200 with a two-octet body.
pub async fn serve_ours(io: DuplexStream, config: Config) -> Result<(), oxiroute_h2::Error> {
    let mut server = Server::handshake(io, config).await?;
    while let Some(r) = server.accept().await {
        let (_parts, mut stream) = r?;
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "2".parse().unwrap());
        stream.send_response(StatusCode::OK, headers, false)?;
        let n = poll_fn(|cx| stream.poll_capacity(cx, 2)).await?;
        assert_eq!(n, 2);
        stream.send_data(Bytes::from_static(b"ok"), true)?;
    }
    Ok(())
}

pub async fn serve_h2(io: DuplexStream, builder: h2::server::Builder) -> Result<(), h2::Error> {
    let mut conn = builder.clone().handshake::<_, Bytes>(io).await?;
    while let Some(r) = conn.accept().await {
        let (_req, mut respond) = r?;
        let response = http::Response::builder()
            .status(200)
            .header("content-length", "2")
            .body(())
            .unwrap();
        let mut send = respond.send_response(response, false)?;
        send.send_data(Bytes::from_static(b"ok"), true)?;
    }
    Ok(())
}

pub fn poll_once<T>(f: impl std::future::Future<Output = T>) -> Option<T> {
    let mut f = std::pin::pin!(f);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}

// ---- application helpers for the interop tests ----

use oxiroute_h2::Error;
use oxiroute_h2::server::Stream;

/// Writes `data` honouring flow control; `end` sets END_STREAM on the last frame.
pub async fn write_body(stream: &mut Stream, mut data: Bytes, end: bool) -> Result<(), Error> {
    if data.is_empty() && end {
        return stream.send_data(data, true);
    }
    while !data.is_empty() {
        let n = poll_fn(|cx| stream.poll_capacity(cx, data.len())).await?;
        let last = n == data.len();
        stream.send_data(data.split_to(n), end && last)?;
    }
    Ok(())
}

/// Reads the whole request body, releasing capacity as it goes.
pub async fn read_body(stream: &mut Stream) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = poll_fn(|cx| stream.poll_data(cx)).await {
        let chunk = chunk?;
        stream.release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub type Handler = fn(
    http::request::Parts,
    Stream,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Accept loop that spawns `handler` per request; returns when the connection ends.
pub async fn serve_with(io: DuplexStream, config: Config, handler: Handler) -> Result<(), Error> {
    let mut server = Server::handshake(io, config).await?;
    while let Some(r) = server.accept().await {
        let (parts, stream) = r?;
        tokio::spawn(handler(parts, stream));
    }
    Ok(())
}

pub fn response(status: u16, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap) {
    let mut map = HeaderMap::new();
    for (n, v) in headers {
        map.append(
            http::HeaderName::from_bytes(n.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    (StatusCode::from_u16(status).unwrap(), map)
}

/// An `h2` client connected to a server running `handler`; returns the client handle and the server task.
pub async fn h2_client(
    config: Config,
    handler: Handler,
) -> (
    h2::client::SendRequest<Bytes>,
    tokio::task::JoinHandle<Result<(), Error>>,
) {
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_with(server_io, config, handler));
    let (send, conn) = h2::client::handshake(client_io).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    (send, server)
}
