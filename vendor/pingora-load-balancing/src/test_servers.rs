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

//! Loopback servers that stand in for the Internet hosts the tests used to dial. Every server owns a port
//! the kernel assigned at bind time and keeps it until it is dropped, so no test names a port before binding
//! it and no port is given back and picked up again.

use std::any::Any;
use std::net::{SocketAddr, TcpListener as StdListener};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use tokio::task::JoinHandle;

const LOOPBACK: &str = "127.0.0.1:0";
const REDIRECT: &[u8] = b"HTTP/1.1 301 Moved Permanently\r\nContent-Length: 0\r\n\r\n";
#[cfg(feature = "openssl")]
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";

/// A bound loopback address and whatever keeps its behavior alive until it is dropped.
pub(crate) struct Loopback {
    pub addr: SocketAddr,
    task: Option<JoinHandle<()>>,
    _keep: Option<Box<dyn Any + Send>>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Answers every request on the stream with `response` until the client closes it.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, response: &[u8]) {
    let mut buf = [0u8; 4096];
    // test requests are header-only, so one read is one request
    while stream.read(&mut buf).await.is_ok_and(|n| n > 0) {
        stream.write_all(response).await.unwrap();
    }
}

/// Accepts every connection and answers each HTTP/1.1 request on it with an empty 301.
pub(crate) fn redirect_server() -> Loopback {
    let listener = StdListener::bind(LOOPBACK).unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = TcpListener::from_std(listener).unwrap();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(serve(stream, REDIRECT));
        }
    });
    Loopback {
        addr,
        task: Some(task),
        _keep: None,
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
        _keep: Some(Box::new(socket)),
    }
}

/// Accepts TLS connections with a generated certificate that no client can verify, and answers every
/// request with an empty 200.
#[cfg(feature = "openssl")]
pub(crate) fn tls_server() -> Loopback {
    use openssl::asn1::Asn1Time;
    use openssl::ec::{EcGroup, EcKey};
    use openssl::hash::MessageDigest;
    use openssl::nid::Nid;
    use openssl::pkey::PKey;
    use openssl::ssl::{Ssl, SslAcceptor, SslMethod};
    use openssl::x509::{X509Builder, X509NameBuilder};
    use pingora_core::tls::tokio_ssl::SslStream;
    use std::pin::Pin;

    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    let key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(Nid::COMMONNAME, "localhost")
        .unwrap();
    let name = name.build();
    let mut cert = X509Builder::new().unwrap();
    cert.set_version(2).unwrap();
    cert.set_subject_name(&name).unwrap();
    cert.set_issuer_name(&name).unwrap();
    cert.set_pubkey(&key).unwrap();
    cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    let mut acceptor = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    acceptor.set_certificate(&cert.build()).unwrap();
    acceptor.set_private_key(&key).unwrap();
    let acceptor = acceptor.build();

    let listener = StdListener::bind(LOOPBACK).unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = TcpListener::from_std(listener).unwrap();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let ssl = Ssl::new(acceptor.context()).unwrap();
            tokio::spawn(async move {
                let mut stream = SslStream::new(ssl, stream).unwrap();
                Pin::new(&mut stream).accept().await.unwrap();
                serve(stream, OK).await;
            });
        }
    });
    Loopback {
        addr,
        task: Some(task),
        _keep: None,
    }
}
