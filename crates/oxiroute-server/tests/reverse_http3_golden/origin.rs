//! The upstream HTTP/3 origin and a QUIC endpoint that refuses every connection.
//!
//! The origin logs the normalised requests it receives and answers according to the `x-origin-mode`
//! request header, so the corpus chooses the upstream behaviour per request.

use std::{
    collections::HashMap,
    fs,
    io::BufReader,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use h3::error::Code;
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response};
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};

use crate::{fixture_support, golden};

const H3_ALPN: &[u8] = b"h3";

type Stream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

const BIG_BODY_BYTES: usize = 200 * 1024;

pub struct Origin {
    pub address: SocketAddr,
    log: Arc<Mutex<Vec<Value>>>,
}

struct State {
    log: Arc<Mutex<Vec<Value>>>,
    hits: Mutex<HashMap<String, u64>>,
}

impl Origin {
    pub fn start() -> Self {
        let address = crate::process_support::ports::unused_address();
        let endpoint = quinn::Endpoint::server(server_config(), address).expect("origin endpoint");
        let log = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(State {
            log: Arc::clone(&log),
            hits: Mutex::default(),
        });
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Ok(connection) = incoming.await {
                        serve_connection(connection, state).await;
                    }
                });
            }
        });
        Self { address, log }
    }

    /// The requests the origin received since the last call.
    pub fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.log.lock().expect("origin log"))
    }
}

/// An endpoint that completes no handshake: connecting to it fails fast with a refusal.
pub struct Refuser {
    pub address: SocketAddr,
    refused: Arc<AtomicU64>,
}

impl Refuser {
    pub fn start() -> Self {
        let address = crate::process_support::ports::unused_address();
        let endpoint =
            quinn::Endpoint::server(server_config(), address).expect("refusing endpoint");
        let refused = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&refused);
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                incoming.refuse();
            }
        });
        Self { address, refused }
    }

    pub fn take(&self) -> u64 {
        self.refused.swap(0, Ordering::SeqCst)
    }
}

fn server_config() -> quinn::ServerConfig {
    let mut certificates = BufReader::new(
        fs::File::open(fixture_support::fixture("origin.pem")).expect("origin certificate"),
    );
    let certificates = CertificateDer::pem_reader_iter(&mut certificates)
        .collect::<Result<Vec<_>, _>>()
        .expect("chain");
    let mut key = BufReader::new(
        fs::File::open(fixture_support::fixture("origin-key.pem")).expect("origin key"),
    );
    let key = PrivateKeyDer::pem_reader_iter(&mut key)
        .next()
        .expect("origin key block")
        .expect("origin key");
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .expect("origin TLS identity");
    crypto.alpn_protocols = vec![H3_ALPN.to_vec()];
    crypto.max_early_data_size = 0;
    quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).expect("origin QUIC TLS"),
    ))
}

async fn serve_connection(connection: quinn::Connection, state: Arc<State>) {
    let Ok(mut h3) = h3::server::builder()
        .build::<_, Bytes>(h3_quinn::Connection::new(connection))
        .await
    else {
        return;
    };
    while let Ok(Some(resolver)) = h3.accept().await {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Ok((request, stream)) = resolver.resolve_request().await {
                handle(&state, request, stream).await;
            }
        });
    }
}

async fn handle(state: &State, request: Request<()>, mut stream: Stream) {
    let mut body = Vec::new();
    while let Ok(Some(mut chunk)) = stream.recv_data().await {
        body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    let trailers = stream.recv_trailers().await.ok().flatten();
    let mut entry = json!({
        "method": request.method().as_str(),
        "uri": golden::scrub(&request.uri().to_string()),
        "headers": golden::headers_json(request.headers().iter().map(|(name, value)| (name.as_str(), value.as_bytes()))),
        "body": golden::body_json(&body),
    });
    if let Some(trailers) = trailers {
        entry["trailers"] = golden::headers_json(
            trailers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
    }
    state.log.lock().expect("origin log").push(entry);
    let mode = request
        .headers()
        .get("x-origin-mode")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("echo");
    match mode {
        "hang" => tokio::time::sleep(Duration::from_secs(60)).await,
        "reset" => stream.stop_stream(Code::H3_INTERNAL_ERROR),
        _ => {
            let head = request.method() == http::Method::HEAD;
            respond(&mut stream, head, reply(state, mode, &request)).await;
        }
    }
}

#[derive(Default)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    chunks: Vec<Vec<u8>>,
    trailers: Vec<(&'static str, &'static str)>,
    /// Omit the automatic `content-length` (declared explicitly, or chunked in spirit).
    own_length: bool,
}

fn text(status: u16, body: &str) -> Reply {
    Reply {
        status,
        chunks: vec![body.as_bytes().to_vec()],
        ..Reply::default()
    }
}

fn hit(state: &State, key: String) -> u64 {
    let mut hits = state.hits.lock().expect("origin hits");
    let count = hits.entry(key).or_default();
    *count += 1;
    *count
}

fn reply(state: &State, mode: &str, request: &Request<()>) -> Reply {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let key = format!(
        "{}{}",
        request
            .uri()
            .authority()
            .map_or("", |authority| authority.as_str()),
        request.uri().path()
    );
    let mut reply = match mode {
        "echo" => Reply {
            headers: vec![("x-origin", "1".into())],
            ..text(200, "origin-ok")
        },
        "empty" => text(200, ""),
        "headers" => Reply {
            headers: [
                ("x-set", "upstream"),
                ("x-add", "upstream"),
                ("x-remove", "stale"),
                ("x-dup", "a"),
                ("x-dup", "b"),
            ]
            .into_iter()
            .chain([
                (
                    "set-cookie",
                    "sid=1; Path=/internal; Secure; SameSite=Strict",
                ),
                ("set-cookie", "other=2; Path=/internal"),
            ])
            .map(|(name, value)| (name, value.to_owned()))
            .collect(),
            ..text(200, "headers")
        },
        "headers-hop" => Reply {
            headers: [
                ("connection", "x-hop"),
                ("x-hop", "dropped"),
                ("keep-alive", "timeout=5"),
                ("x-kept", "yes"),
            ]
            .map(|(name, value)| (name, value.to_owned()))
            .to_vec(),
            ..text(200, "hop")
        },
        "trailers" => Reply {
            trailers: vec![("x-origin-trailer", "complete"), ("x-second", "two")],
            ..text(200, "with-trailers")
        },
        "trailers-hop" => Reply {
            trailers: vec![
                ("connection", "close"),
                ("content-length", "3"),
                ("x-kept", "yes"),
            ],
            ..text(200, "bad-trailers")
        },
        "big" => Reply {
            chunks: vec![
                (0..BIG_BODY_BYTES)
                    .map(|index| (index % 251) as u8)
                    .collect(),
            ],
            ..text(200, "")
        },
        "mismatch" => Reply {
            headers: vec![("content-length", "10".into())],
            own_length: true,
            ..text(200, "short")
        },
        "nolength" => Reply {
            own_length: true,
            chunks: vec![b"chunk-a".to_vec(), b"chunk-b".to_vec()],
            ..text(200, "")
        },
        "bighdr" => Reply {
            headers: vec![("x-big", "a".repeat(17_000))],
            ..text(200, "bighdr")
        },
        "dupcl" => Reply {
            headers: vec![
                ("content-length", "5".into()),
                ("content-length", "5".into()),
            ],
            own_length: true,
            ..text(200, "dupcl")
        },
        "badcl" => Reply {
            headers: vec![("content-length", "abc".into())],
            own_length: true,
            ..text(200, "badcl")
        },
        "flaky" if hit(state, header("x-flaky-key").unwrap_or_default().to_owned()) == 1 => {
            text(503, "flaky-503")
        }
        "flaky" => text(200, "recovered"),
        "cache-fresh" => cached(request, &key, "public, max-age=60", hit(state, key.clone())),
        "cache-stale" => cached(request, &key, "max-age=0", hit(state, key.clone())),
        "cache-nostore" => cached(request, &key, "no-store", hit(state, key.clone())),
        "cache-err" if hit(state, key.clone()) == 1 => cached(request, &key, "max-age=0", 1),
        "cache-err" => text(500, "cache-err-500"),
        status if status.starts_with("status-") => {
            let code = status[7..].parse().expect("status mode");
            let body = if matches!(code, 204 | 205 | 304) {
                String::new()
            } else {
                format!("status-{code}")
            };
            Reply {
                headers: vec![("x-origin-status", code.to_string())],
                ..text(code, &body)
            }
        }
        other => panic!("unknown origin mode {other}"),
    };
    if reply.status == 0 {
        reply.status = 200;
    }
    reply
}

/// A cacheable representation with validators; honours `If-None-Match`.
fn cached(request: &Request<()>, key: &str, cache_control: &str, hits: u64) -> Reply {
    let etag = format!("\"{}\"", key.rsplit('/').next().unwrap_or("v"));
    let validators = [
        ("etag", etag.clone()),
        ("last-modified", "Sat, 01 Jan 2022 00:00:00 GMT".into()),
    ];
    let mut headers = vec![
        ("cache-control", cache_control.to_owned()),
        ("surrogate-key", "tag-a tag-b".into()),
    ];
    headers.extend(validators);
    let matches_etag = request
        .headers()
        .get("if-none-match")
        .is_some_and(|value| value.as_bytes() == etag.as_bytes());
    if matches_etag {
        return Reply {
            headers,
            ..text(304, "")
        };
    }
    Reply {
        headers,
        ..text(200, &format!("cached-{hits}"))
    }
}

async fn respond(stream: &mut Stream, head: bool, reply: Reply) {
    // The proxy closes the connection when it has what it needs, so sending may fail.
    let _ = send(stream, head, reply).await;
}

async fn send(stream: &mut Stream, head: bool, reply: Reply) -> Result<(), h3::error::StreamError> {
    let body = reply.chunks.concat();
    let mut response = Response::builder().status(reply.status);
    for (name, value) in &reply.headers {
        response = response.header(*name, value);
    }
    if !reply.own_length && !matches!(reply.status, 204 | 205 | 304) {
        response = response.header("content-length", body.len());
    }
    stream
        .send_response(response.body(()).expect("origin response"))
        .await?;
    if !head {
        for chunk in reply.chunks.into_iter().filter(|chunk| !chunk.is_empty()) {
            stream.send_data(Bytes::from(chunk)).await?;
        }
        if !reply.trailers.is_empty() {
            let mut trailers = HeaderMap::new();
            for (name, value) in reply.trailers {
                trailers.append(
                    HeaderName::from_static(name),
                    HeaderValue::from_static(value),
                );
            }
            stream.send_trailers(trailers).await?;
        }
    }
    stream.finish().await
}
