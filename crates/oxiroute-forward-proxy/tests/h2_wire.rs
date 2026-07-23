mod support;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use oxiroute_forward_proxy::{Decision, Protocol};
use tokio::net::{TcpListener, TcpStream};

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
                            format!("tunnel:{}", tunnel.destination.destination.authority()),
                        )
                        .body(())
                        .expect("H2 CONNECT response");
                    let mut response_body = respond
                        .send_response(response, false)
                        .expect("H2 CONNECT response frames");
                    let mut request_body = request.into_body();
                    let payload = request_body
                        .data()
                        .await
                        .expect("H2 CONNECT payload frame")
                        .expect("H2 CONNECT payload");
                    assert_eq!(&payload[..], b"ping");
                    response_body
                        .send_data(Bytes::from_static(b"pong"), true)
                        .expect("H2 CONNECT echo frame");
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
    let echo = response_body
        .data()
        .await
        .expect("H2 tunnel echo frame")
        .expect("H2 tunnel echo");
    assert_eq!(&echo[..], b"pong");

    drop(client);
    server.await.expect("H2 server task");
    client_driver.await.expect("H2 driver task");
}
