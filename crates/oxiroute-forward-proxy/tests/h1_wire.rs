mod support;

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use oxiroute_forward_proxy::{Decision, Protocol};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

#[tokio::test]
async fn h1_absolute_form_crosses_a_real_http_connection() {
    let response = exchange(
        b"GET http://example.com:80/h1?q=wire HTTP/1.1\r\n\
          Host: stale.invalid\r\n\
          Proxy-Authorization: Bearer wire-test\r\n\
          Connection: close, x-remove\r\n\
          X-Remove: private\r\n\
          X-End-To-End: kept\r\n\r\n",
    )
    .await;

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("forward:/h1?q=wire:example.com:80"));
}

#[tokio::test]
async fn h1_connect_ipv6_authority_crosses_a_real_http_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("H1 bind");
    let address = listener.local_addr().expect("H1 listener address");
    let (upgraded_tx, upgraded_rx) = oneshot::channel();
    let upgraded_tx = Arc::new(Mutex::new(Some(upgraded_tx)));
    let server = tokio::spawn(async move {
        let (stream, client_addr) = listener.accept().await.expect("H1 accept");
        http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |mut request: Request<Incoming>| {
                    let upgraded_tx = Arc::clone(&upgraded_tx);
                    async move {
                        let decision = support::decide(Protocol::Http1, client_addr, &request)
                            .await
                            .expect("H1 CONNECT decision");
                        let Decision::Tunnel(tunnel) = decision else {
                            panic!("H1 CONNECT must produce a tunnel");
                        };
                        assert_eq!(
                            tunnel.destination.destination.authority(),
                            "[2606:4700:4700::1111]:443"
                        );
                        let upgrade = hyper::upgrade::on(&mut request);
                        let upgraded_tx = upgraded_tx
                            .lock()
                            .expect("H1 upgrade sender lock")
                            .take()
                            .expect("one H1 upgrade");
                        tokio::spawn(async move {
                            let mut upgraded = TokioIo::new(upgrade.await.expect("H1 upgrade"));
                            let mut payload = [0; 4];
                            upgraded
                                .read_exact(&mut payload)
                                .await
                                .expect("H1 tunnel payload");
                            assert_eq!(&payload, b"ping");
                            upgraded.write_all(b"pong").await.expect("H1 tunnel echo");
                            upgraded.shutdown().await.expect("H1 tunnel shutdown");
                            upgraded_tx.send(()).ok();
                        });
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(200)
                                .body(Full::new(Bytes::new()))
                                .expect("H1 CONNECT response"),
                        )
                    }
                }),
            )
            .with_upgrades()
            .await
            .expect("H1 serve upgraded connection");
        upgraded_rx.await.expect("H1 upgraded task");
    });

    let mut client = TcpStream::connect(address).await.expect("H1 connect");
    client
        .write_all(
            b"CONNECT [2606:4700:4700::1111]:443 HTTP/1.1\r\n\
              Host: [2606:4700:4700::1111]:443\r\n\
              Proxy-Authorization: Bearer wire-test\r\n\r\nping",
        )
        .await
        .expect("H1 CONNECT and over-read payload");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("H1 CONNECT response and tunnel echo");
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("H1 response header terminator")
        + 4;
    assert!(response[..split].starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert_eq!(&response[split..], b"pong");
    server.await.expect("H1 CONNECT server task");
}

async fn exchange(request: &[u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("H1 bind");
    let address = listener.local_addr().expect("H1 listener address");
    let server = tokio::spawn(async move {
        let (stream, client_addr) = listener.accept().await.expect("H1 accept");
        http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |request: Request<Incoming>| async move {
                    let decision = support::decide(Protocol::Http1, client_addr, &request)
                        .await
                        .expect("H1 decision");
                    let body = match decision {
                        Decision::Forward(forward) => {
                            assert!(!forward.headers.contains_key("x-remove"));
                            assert_eq!(forward.headers["x-end-to-end"], "kept");
                            format!(
                                "forward:{}:{}",
                                forward.target,
                                forward.destination.destination.authority()
                            )
                        }
                        Decision::Tunnel(tunnel) => {
                            format!("tunnel:{}", tunnel.destination.destination.authority())
                        }
                    };
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("x-proxy-decision", &body)
                            .body(Full::new(Bytes::from(body)))
                            .expect("H1 response"),
                    )
                }),
            )
            .await
            .expect("H1 serve connection");
    });

    let mut client = TcpStream::connect(address).await.expect("H1 connect");
    client.write_all(request).await.expect("H1 request write");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("H1 response read");
    server.await.expect("H1 server task");
    String::from_utf8(response).expect("H1 response text")
}
