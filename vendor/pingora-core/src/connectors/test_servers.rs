// Copyright 2025 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Loopback servers that stand in for the Internet hosts the connector tests used to dial. Every server
//! owns a port the kernel assigned at bind time and keeps it until it is dropped, so no test names a port
//! before binding it and no port is given back and picked up again.

use std::any::Any;
use std::net::{SocketAddr, TcpListener as StdListener, TcpStream as StdStream};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use tokio::task::JoinHandle;

const LOOPBACK: &str = "127.0.0.1:0";
const REDIRECT: &[u8] = b"HTTP/1.1 301 Moved Permanently\r\nContent-Length: 0\r\n\r\n";
#[cfg(feature = "openssl")]
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";

/// Answers every request on the stream with `response` until the client closes it.
async fn serve_h1<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, response: &[u8]) {
    let mut buf = [0u8; 4096];
    // test requests are header-only, so one read is one request
    while stream.read(&mut buf).await.is_ok_and(|n| n > 0) {
        stream.write_all(response).await.unwrap();
        // pingora streams coalesce writes until flushed
        stream.flush().await.unwrap();
    }
}

/// A bound loopback address and whatever keeps its behavior (accept loop, sockets) alive until it is dropped.
pub(crate) struct Loopback {
    pub addr: SocketAddr,
    task: Option<JoinHandle<()>>,
    _keep: Vec<Box<dyn Any + Send>>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Answers every HTTP/1.1 request on every connection with an empty 301 and keeps the connection alive.
pub(crate) fn redirect_server() -> Loopback {
    let listener = StdListener::bind(LOOPBACK).unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = TcpListener::from_std(listener).unwrap();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(serve_h1(stream, REDIRECT));
        }
    });
    Loopback {
        addr,
        task: Some(task),
        _keep: vec![],
    }
}

/// An address whose connections the kernel refuses: the port is bound, so nobody else can take it, but it
/// never listens.
pub(crate) fn refused() -> Loopback {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind(LOOPBACK.parse().unwrap()).unwrap();
    Loopback {
        addr: socket.local_addr().unwrap(),
        task: None,
        _keep: vec![Box::new(socket)],
    }
}

/// An address whose connect never completes: the listener's accept queue is full, so the kernel drops
/// further SYNs.
pub(crate) fn blackhole() -> Loopback {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind(LOOPBACK.parse().unwrap()).unwrap();
    let listener = socket.listen(1).unwrap();
    let addr = listener.local_addr().unwrap();
    // a backlog of 1 queues two connections that nobody accepts
    let mut keep: Vec<Box<dyn Any + Send>> = vec![Box::new(listener)];
    keep.extend((0..2).map(|_| Box::new(StdStream::connect(addr).unwrap()) as _));
    Loopback {
        addr,
        task: None,
        _keep: keep,
    }
}

#[cfg(feature = "openssl")]
pub(crate) use tls::*;

/// TLS servers with a generated certificate. Clients must not verify it: set `verify_cert` and
/// `verify_hostname` to false on the peer.
#[cfg(feature = "openssl")]
mod tls {
    use super::*;
    use crate::listeners::tls::TlsSettings;
    use crate::listeners::{Listeners, TransportStack, ALPN};
    use crate::test_tls::TestCert;
    use crate::upstreams::peer::HttpPeer;

    /// A TLS listener on a kernel-assigned port and the certificate it presents.
    pub(crate) struct TlsListener {
        pub stack: TransportStack,
        pub addr: SocketAddr,
        _cert: TestCert,
    }

    pub(crate) async fn tls_listener(alpn: ALPN) -> TlsListener {
        let cert = TestCert::new();
        let mut settings = TlsSettings::intermediate(&cert.cert_path, &cert.key_path).unwrap();
        settings.set_alpn(alpn);
        let mut listeners = Listeners::new();
        listeners.add_tls_with_settings(LOOPBACK, None, settings);
        let stack = listeners.build(None).await.unwrap().pop().unwrap();
        TlsListener {
            addr: stack.tcp_addr(),
            stack,
            _cert: cert,
        }
    }

    /// A TLS peer for a test server, which presents a certificate no client can verify.
    pub(crate) fn tls_peer(server: &Loopback) -> HttpPeer {
        let mut peer = HttpPeer::new(server.addr, true, "localhost".into());
        peer.options.verify_cert = false;
        peer.options.verify_hostname = false;
        peer
    }

    async fn serve_h2<S: AsyncRead + AsyncWrite + Unpin>(stream: S) {
        let mut conn = h2::server::handshake(stream).await.unwrap();
        while let Some(Ok((_request, mut respond))) = conn.accept().await {
            respond
                .send_response(http::Response::new(()), true)
                .unwrap();
        }
    }

    /// Negotiates `alpn` and answers HTTP/1.1 requests, or HTTP/2 requests when h2 is chosen, with an empty 200.
    pub(crate) async fn tls_server(alpn: ALPN) -> Loopback {
        let listener = tls_listener(alpn).await;
        let addr = listener.addr;
        let task = tokio::spawn(async move {
            loop {
                let stream = listener.stack.accept().await.unwrap();
                tokio::spawn(async move {
                    let stream = stream.handshake().await.unwrap();
                    match stream.selected_alpn_proto() {
                        Some(ALPN::H2) => serve_h2(stream).await,
                        _ => serve_h1(stream, OK).await,
                    }
                });
            }
        });
        Loopback {
            addr,
            task: Some(task),
            _keep: vec![],
        }
    }
}
