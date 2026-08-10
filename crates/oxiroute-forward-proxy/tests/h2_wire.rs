mod support;

use std::{future::pending, io};

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use oxiroute_forward_proxy::{BoundedTunnel, Decision, H2TunnelStream, Protocol, TunnelLimits};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

struct H2WireStream {
    receive: h2::RecvStream,
    send: h2::SendStream<Bytes>,
}

#[async_trait::async_trait]
impl H2TunnelStream for H2WireStream {
    async fn recv_data(&mut self) -> io::Result<Option<Bytes>> {
        let data = self
            .receive
            .data()
            .await
            .transpose()
            .map_err(io::Error::other)?;
        if let Some(data) = &data {
            self.receive
                .flow_control()
                .release_capacity(data.len())
                .map_err(io::Error::other)?;
        }
        Ok(data)
    }

    async fn send_data(&mut self, mut data: Bytes, end: bool) -> io::Result<()> {
        if data.is_empty() {
            self.send.send_data(data, end).map_err(io::Error::other)?;
            return Ok(());
        }
        while !data.is_empty() {
            self.send.reserve_capacity(data.len());
            let capacity = std::future::poll_fn(|context| self.send.poll_capacity(context))
                .await
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "H2 send closed"))?
                .map_err(io::Error::other)?;
            let length = capacity.min(data.len());
            let chunk = data.split_to(length);
            self.send
                .send_data(chunk, data.is_empty() && end)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }

    async fn wait_closed(&mut self) -> io::Result<()> {
        pending().await
    }

    async fn reset(&mut self) {
        self.send.send_reset(h2::Reason::CANCEL);
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn h2_forward_and_connect_cross_real_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("H2 bind");
    let address = listener.local_addr().expect("H2 listener address");
    let server = tokio::spawn(async move {
        let (stream, client_addr) = listener.accept().await.expect("H2 accept");
        let mut connection = h2::server::handshake(stream)
            .await
            .expect("H2 server handshake");
        for _ in 0..2 {
            let (request, mut respond) = connection
                .accept()
                .await
                .expect("H2 connection")
                .expect("H2 request");
            let decision = support::decide(Protocol::Http2, client_addr, &request)
                .await
                .expect("H2 decision");
            match decision {
                Decision::Forward(forward) => {
                    let response = Response::builder()
                        .status(StatusCode::OK)
                        .header("x-proxy-decision", format!("forward:{}", forward.target))
                        .body(())
                        .expect("H2 response");
                    respond
                        .send_response(response, true)
                        .expect("H2 response frames");
                }
                Decision::Tunnel(tunnel) => {
                    let response = Response::builder()
                        .status(StatusCode::OK)
                        .header(
                            "x-proxy-decision",
                            format!("tunnel:{}", tunnel.destination.destination().authority()),
                        )
                        .body(())
                        .expect("H2 CONNECT response");
                    let response_body = respond
                        .send_response(response, false)
                        .expect("H2 CONNECT response frames");
                    let receive = request.into_body();
                    let (proxy_upstream, mut origin) = tokio::io::duplex(64);
                    let origin_task = tokio::spawn(async move {
                        let mut payload = [0; 4];
                        origin
                            .read_exact(&mut payload)
                            .await
                            .expect("H2 CONNECT origin payload");
                        assert_eq!(&payload, b"ping");
                        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                        origin
                            .write_all(b"po")
                            .await
                            .expect("H2 CONNECT origin response");
                        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                        origin
                            .write_all(b"ng")
                            .await
                            .expect("H2 CONNECT origin response");
                        origin.shutdown().await.expect("H2 CONNECT origin shutdown");
                    });
                    let outcome = BoundedTunnel::new(TunnelLimits {
                        max_bytes_per_direction: 64,
                        idle_timeout: std::time::Duration::from_secs(1),
                        lifetime_timeout: std::time::Duration::from_secs(2),
                        buffer_size: 16,
                    })
                    .expect("H2 tunnel limits")
                    .relay_h2(
                        H2WireStream {
                            receive,
                            send: response_body,
                        },
                        proxy_upstream,
                    )
                    .await;
                    origin_task.await.expect("H2 CONNECT origin task");
                    assert!(matches!(
                        outcome,
                        oxiroute_forward_proxy::TunnelOutcome::Ended {
                            end: oxiroute_forward_proxy::TunnelEnd::Eof,
                            ..
                        }
                    ));
                }
            }
        }
        connection.graceful_shutdown();
        while let Some(request) = connection.accept().await {
            request.expect("H2 graceful shutdown");
        }
    });

    let stream = TcpStream::connect(address).await.expect("H2 connect");
    let (mut client, connection) = h2::client::handshake(stream)
        .await
        .expect("H2 client handshake");
    let client_driver = tokio::spawn(async move { connection.await.expect("H2 client driver") });

    let forward = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/h2?q=wire")
        .header("proxy-authorization", "Bearer wire-test")
        .body(())
        .expect("H2 forward request");
    let (response, _) = client.send_request(forward, true).expect("send H2 forward");
    let response = response.await.expect("receive H2 forward");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-proxy-decision"], "forward:/h2?q=wire");

    let connect = Request::builder()
        .method(Method::CONNECT)
        .uri("example.com:443")
        .header("proxy-authorization", "Bearer wire-test")
        .body(())
        .expect("H2 CONNECT request");
    let (response, mut request_body) = client
        .send_request(connect, false)
        .expect("send H2 CONNECT");
    request_body
        .send_data(Bytes::from_static(b"ping"), true)
        .expect("send H2 tunnel payload");
    let response = response.await.expect("receive H2 CONNECT");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-proxy-decision"],
        "tunnel:example.com:443"
    );
    let mut response_body = response.into_body();
    let mut echo = Vec::new();
    while echo.len() < 4 {
        let data = response_body
            .data()
            .await
            .expect("H2 tunnel echo frame")
            .expect("H2 tunnel echo");
        echo.extend_from_slice(&data);
    }
    assert_eq!(&echo[..], b"pong");

    drop(client);
    server.await.expect("H2 server task");
    client_driver.await.expect("H2 driver task");
}
