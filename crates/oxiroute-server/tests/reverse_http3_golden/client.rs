//! The `h3` crate client: ordinary requests and what the server answered, as normalised observations.

use std::{net::SocketAddr, time::Duration};

use bytes::{Buf as _, Bytes};
use h3::{
    error::{ConnectionError, StreamError},
    quic::ConnectionErrorIncoming,
};
use http::{HeaderMap, HeaderName, HeaderValue, Request};
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

use crate::{golden, support};

const STEP: Duration = Duration::from_secs(8);
const SETTLE: Duration = Duration::from_millis(50);

pub enum Body {
    None,
    Bytes(Vec<u8>),
    Chunks(Vec<Vec<u8>>),
    /// Sent, but the request stream is never finished.
    Stall(Vec<u8>),
}

pub struct Req {
    pub method: &'static str,
    pub host: Option<String>,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    pub trailers: Vec<(String, String)>,
}

impl Req {
    pub fn new(method: &'static str, path: &str) -> Self {
        Self {
            method,
            host: None,
            path: path.into(),
            headers: Vec::new(),
            body: Body::None,
            trailers: Vec::new(),
        }
    }

    pub fn get(path: &str) -> Self {
        Self::new("GET", path)
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Chooses the upstream behaviour of the origin.
    pub fn mode(self, mode: &str) -> Self {
        self.header("x-origin-mode", mode)
    }

    pub fn host(mut self, host: &str) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    pub fn text(self, body: &str) -> Self {
        self.body(Body::Bytes(body.as_bytes().to_vec()))
    }

    pub fn trailer(mut self, name: &str, value: &str) -> Self {
        self.trailers.push((name.into(), value.into()));
        self
    }

    pub fn describe(&self, host: &str) -> String {
        format!(
            "{} https://{}{}",
            self.method,
            self.host.as_deref().unwrap_or(host),
            self.path
        )
    }
}

pub struct Session {
    pub connection: quinn::Connection,
    sender: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    driver: tokio::task::JoinHandle<()>,
}

impl Session {
    pub async fn connect(endpoint: &quinn::Endpoint, address: SocketAddr) -> Self {
        let connection = connect_quic(endpoint, address).await;
        let (mut driver, sender) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
            .await
            .expect("H3 client connection");
        let driver = tokio::spawn(async move {
            let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
        });
        Self {
            connection,
            sender,
            driver,
        }
    }

    pub async fn exchange(&mut self, host: &str, req: &Req) -> Value {
        let uri = format!(
            "https://{}{}",
            req.host.as_deref().unwrap_or(host),
            req.path
        );
        let mut builder = Request::builder().method(req.method).uri(uri);
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        let request = builder.body(()).expect("golden request");
        let mut stream = match timeout(STEP, self.sender.send_request(request)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => return json!({ "send_request": error_json(&error) }),
            Err(_) => return json!({ "send_request": "timeout" }),
        };
        // Send failures are ignored: the server may answer and cancel before the body is written.
        match &req.body {
            Body::None => {}
            Body::Chunks(chunks) => {
                for chunk in chunks {
                    drop(stream.send_data(Bytes::from(chunk.clone())).await);
                    sleep(Duration::from_millis(20)).await;
                }
            }
            Body::Bytes(bytes) | Body::Stall(bytes) => {
                drop(stream.send_data(Bytes::from(bytes.clone())).await);
            }
        }
        if !matches!(req.body, Body::Stall(_)) {
            if req.trailers.is_empty() {
                drop(stream.finish().await);
            } else {
                drop(stream.send_trailers(header_map(&req.trailers)).await);
            }
        }
        read_response(&mut stream).await
    }

    /// `"open"`, or how the connection was closed; sampled after a moment for late closes.
    pub async fn state(&self) -> Value {
        sleep(SETTLE).await;
        connection_state(&self.connection)
    }

    pub fn close(self) {
        self.connection
            .close(quinn::VarInt::from_u32(0), b"golden case complete");
        self.driver.abort();
    }
}

async fn read_response(
    stream: &mut h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
) -> Value {
    let response = match timeout(STEP, stream.recv_response()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return error_json(&error),
        Err(_) => return json!({ "timeout": true }),
    };
    let mut body = Vec::new();
    let mut failure = None;
    loop {
        match timeout(STEP, stream.recv_data()).await {
            Ok(Ok(Some(mut chunk))) => {
                body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
            }
            Ok(Ok(None)) => break,
            Ok(Err(error)) => failure = Some(error_json(&error)),
            Err(_) => failure = Some(json!({ "timeout": true })),
        }
        if failure.is_some() {
            break;
        }
    }
    let mut outcome = json!({
        "status": response.status().as_u16(),
        "headers": headers_of(response.headers()),
        "body": golden::body_json(&body),
    });
    match (failure, timeout(STEP, stream.recv_trailers()).await) {
        (Some(failure), _) => outcome["body_error"] = failure,
        (None, Ok(Ok(Some(trailers)))) => outcome["trailers"] = headers_of(&trailers),
        (None, Ok(Ok(None))) => {}
        (None, Ok(Err(error))) => outcome["trailers_error"] = error_json(&error),
        (None, Err(_)) => outcome["trailers_error"] = json!({ "timeout": true }),
    }
    outcome
}

pub fn headers_of(headers: &HeaderMap) -> Value {
    golden::headers_json(
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    )
}

fn header_map(pairs: &[(String, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            name.parse::<HeaderName>().expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    map
}

fn error_json(error: &StreamError) -> Value {
    match error {
        StreamError::RemoteTerminate { code } => json!({ "stream_reset": code.value() }),
        StreamError::StreamError { code, .. } => json!({ "stream_error": code.value() }),
        StreamError::ConnectionError(ConnectionError::Remote(
            ConnectionErrorIncoming::ApplicationClose { error_code },
        )) => {
            json!({ "connection_closed": error_code })
        }
        StreamError::ConnectionError(ConnectionError::Remote(
            ConnectionErrorIncoming::ConnectionClosed { error_code },
        )) => {
            json!({ "transport_closed": error_code })
        }
        StreamError::ConnectionError(ConnectionError::Timeout) => {
            json!({ "connection_error": "timeout" })
        }
        StreamError::ConnectionError(_) => json!({ "connection_error": "other" }),
        StreamError::HeaderTooBig { .. } => json!({ "header_too_big": true }),
        StreamError::RemoteClosing => json!({ "remote_closing": true }),
        _ => json!({ "error": "other" }),
    }
}

pub fn connection_state(connection: &quinn::Connection) -> Value {
    match connection.close_reason() {
        None => json!("open"),
        Some(quinn::ConnectionError::ApplicationClosed(close)) => {
            json!({ "closed": close.error_code.into_inner() })
        }
        Some(quinn::ConnectionError::ConnectionClosed(close)) => {
            json!({ "transport_closed": close.error_code.to_string() })
        }
        Some(other) => {
            json!({ "closed_by": format!("{other:?}").split('(').next().unwrap_or_default() })
        }
    }
}

pub fn client_endpoint() -> quinn::Endpoint {
    support::h3::client_endpoint(
        &crate::fixture_support::fixture("ca-a.pem"),
        support::h3::H3_ALPN,
    )
    .expect("H3 client endpoint")
}

pub async fn connect_quic(endpoint: &quinn::Endpoint, address: SocketAddr) -> quinn::Connection {
    support::h3::connect(endpoint, address, support::PROXY_SERVER_NAME).await
}
