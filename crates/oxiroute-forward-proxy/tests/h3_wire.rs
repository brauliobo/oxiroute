#[path = "support/h3_fixture.rs"]
mod h3_fixture;
mod support;

use std::{future::poll_fn, net::SocketAddr, time::Duration};

use bytes::{Buf as _, Bytes, BytesMut};
use h3::{
    error::Code,
    proto::coding::BufMutExt as _,
    qpack::{self, HeaderField},
};
use http::{Method, Request, Response, StatusCode};
use oxiroute_forward_proxy::{
    BoundedTunnel, Decision, DecisionError, H3_ALPN, H3RequestError, Protocol, TunnelEnd,
    TunnelLimits, TunnelOutcome, TunnelStats,
};
use quinn::crypto::rustls::HandshakeData;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::oneshot,
    time::timeout,
};

#[tokio::test]
async fn h3_absolute_target_crosses_quic_tls_and_h3_frames() {
    let fixture = h3_fixture::endpoints();
    let server_endpoint = fixture.server;
    let server = tokio::spawn(async move {
        let connection = accept_server(&server_endpoint).await;
        assert_h3_alpn(&connection);
        let mut h3: h3::server::Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 server connection");
        let resolver = h3
            .accept()
            .await
            .expect("H3 accept")
            .expect("H3 request stream");
        let (request, mut stream) = resolver.resolve_request().await.expect("H3 request");
        let decision = support::decide(Protocol::Http3, client_addr(), &request)
            .await
            .expect("H3 forward decision");
        let Decision::Forward(forward) = decision else {
            panic!("H3 request must be forwarded");
        };
        assert_eq!(
            forward.destination.destination().authority(),
            "example.com:80"
        );
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("x-origin-form", forward.target.to_string())
                    .body(())
                    .expect("H3 response"),
            )
            .await
            .expect("H3 response headers");
        stream.finish().await.expect("H3 response finish");
        let _ = h3.accept().await;
    });

    let connection = connect_client(&fixture.client, fixture.server_address).await;
    assert_h3_alpn(&connection);
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/h3?q=wire")
        .header("proxy-authorization", "Bearer wire-test")
        .body(())
        .expect("H3 request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("receive H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-origin-form"], "/h3?q=wire");

    drop(stream);
    drop(sender);
    server.await.expect("H3 server task");
    finish_driver(driver).await;
}

#[tokio::test]
async fn classic_connect_is_authority_only_and_relays_bidirectionally_across_half_closes() {
    let fixture = h3_fixture::endpoints();
    let server_endpoint = fixture.server;
    let (outcome_tx, outcome_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let connection = accept_server(&server_endpoint).await;
        assert_h3_alpn(&connection);
        let mut h3: h3::server::Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 server connection");
        let resolver = h3
            .accept()
            .await
            .expect("H3 accept")
            .expect("H3 CONNECT stream");
        let (request, mut stream) = resolver
            .resolve_request()
            .await
            .expect("authority-only CONNECT");
        assert_eq!(request.method(), Method::CONNECT);
        assert_eq!(
            request.uri().authority().map(http::uri::Authority::as_str),
            Some("example.com:443")
        );
        assert!(request.uri().scheme().is_none());
        assert!(request.uri().path_and_query().is_none());
        let decision = support::decide(Protocol::Http3, client_addr(), &request)
            .await
            .expect("H3 CONNECT decision");
        assert!(matches!(decision, Decision::Tunnel(_)));
        stream
            .send_response(success_response())
            .await
            .expect("CONNECT response");

        let (proxy_upstream, mut origin) = tokio::io::duplex(256);
        let origin_task = tokio::spawn(async move {
            let mut request = [0; 4];
            origin
                .read_exact(&mut request)
                .await
                .expect("origin request");
            assert_eq!(&request, b"ping");
            origin.write_all(b"pong").await.expect("origin response");
            let mut after_half_close = Vec::new();
            origin
                .read_to_end(&mut after_half_close)
                .await
                .expect("origin request half-close");
            assert!(after_half_close.is_empty());
            origin
                .write_all(b"after-fin")
                .await
                .expect("origin post-FIN response");
            origin.shutdown().await.expect("origin half-close");
        });
        let outcome = tunnel(64).relay_h3(stream, proxy_upstream).await;
        origin_task.await.expect("origin task");
        outcome_tx.send(outcome).ok();
        let _ = h3.accept().await;
    });

    let connection = connect_client(&fixture.client, fixture.server_address).await;
    assert_h3_alpn(&connection);
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut stream = sender
        .send_request(connect_request("example.com:443", true))
        .await
        .expect("send classic CONNECT");
    let response = stream.recv_response().await.expect("CONNECT response");
    assert_eq!(response.status(), StatusCode::OK);
    stream
        .send_data(Bytes::from_static(b"ping"))
        .await
        .expect("client tunnel data");
    assert_eq!(recv_chunk(&mut stream).await.as_ref(), b"pong");
    stream.finish().await.expect("client request half-close");
    assert_eq!(recv_chunk(&mut stream).await.as_ref(), b"after-fin");
    assert!(stream.recv_data().await.expect("response FIN").is_none());

    assert!(matches!(
        outcome_rx.await.expect("tunnel outcome"),
        TunnelOutcome::Ended {
            end: TunnelEnd::Eof,
            stats: TunnelStats {
                left_to_right: 4,
                right_to_left: 13,
            },
        }
    ));
    drop(stream);
    drop(sender);
    server.await.expect("H3 server task");
    finish_driver(driver).await;
}

#[tokio::test]
async fn classic_connect_enforces_the_shared_direction_limit() {
    let fixture = h3_fixture::endpoints();
    let server_endpoint = fixture.server;
    let (outcome_tx, outcome_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let connection = accept_server(&server_endpoint).await;
        let mut h3: h3::server::Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 server connection");
        let resolver = h3.accept().await.unwrap().unwrap();
        let (request, mut stream) = resolver.resolve_request().await.expect("CONNECT request");
        assert!(matches!(
            support::decide(Protocol::Http3, client_addr(), &request).await,
            Ok(Decision::Tunnel(_))
        ));
        stream
            .send_response(success_response())
            .await
            .expect("CONNECT response");
        let (proxy_upstream, mut origin) = tokio::io::duplex(64);
        let origin_task = tokio::spawn(async move {
            let mut bytes = [0; 4];
            origin
                .read_exact(&mut bytes)
                .await
                .expect("bounded origin read");
            assert_eq!(&bytes, b"1234");
        });
        let outcome = tunnel(4).relay_h3(stream, proxy_upstream).await;
        origin_task.await.expect("origin task");
        outcome_tx.send(outcome).ok();
        let _ = h3.accept().await;
    });

    let connection = connect_client(&fixture.client, fixture.server_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut stream = sender
        .send_request(connect_request("example.com:443", true))
        .await
        .expect("CONNECT request");
    assert_eq!(
        stream
            .recv_response()
            .await
            .expect("CONNECT response")
            .status(),
        StatusCode::OK
    );
    stream
        .send_data(Bytes::from_static(b"123456"))
        .await
        .expect("over-limit DATA frame");
    assert!(matches!(
        outcome_rx.await.expect("limit outcome"),
        TunnelOutcome::Ended {
            end: TunnelEnd::ByteLimitLeftToRight,
            stats: TunnelStats {
                left_to_right: 4,
                ..
            },
        }
    ));
    drop(stream);
    drop(sender);
    server.await.expect("H3 server task");
    finish_driver(driver).await;
}

#[tokio::test]
async fn classic_connect_maps_authentication_and_policy_rejections_without_opening_tunnels() {
    let fixture = h3_fixture::endpoints();
    let server_endpoint = fixture.server;
    let server = tokio::spawn(async move {
        let connection = accept_server(&server_endpoint).await;
        let mut h3: h3::server::Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 server connection");
        for expected in [
            StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            StatusCode::FORBIDDEN,
        ] {
            let resolver = h3.accept().await.unwrap().unwrap();
            let (request, mut stream) = resolver.resolve_request().await.expect("CONNECT request");
            let error = support::decide(Protocol::Http3, client_addr(), &request)
                .await
                .expect_err("request rejection");
            let response = rejection_response(&error);
            assert_eq!(response.status(), expected);
            stream
                .send_response(response)
                .await
                .expect("rejection response");
            stream.finish().await.expect("rejection finish");
        }
        let _ = h3.accept().await;
    });

    let connection = connect_client(&fixture.client, fixture.server_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut missing_auth = sender
        .send_request(connect_request("example.com:443", false))
        .await
        .expect("missing-auth CONNECT");
    missing_auth.finish().await.expect("request finish");
    let response = missing_auth.recv_response().await.expect("407 response");
    assert_eq!(response.status(), StatusCode::PROXY_AUTHENTICATION_REQUIRED);
    assert!(
        response
            .headers()
            .contains_key(http::header::PROXY_AUTHENTICATE)
    );

    let mut forbidden = sender
        .send_request(connect_request("127.0.0.1:443", true))
        .await
        .expect("forbidden CONNECT");
    forbidden.finish().await.expect("request finish");
    assert_eq!(
        forbidden
            .recv_response()
            .await
            .expect("403 response")
            .status(),
        StatusCode::FORBIDDEN
    );
    drop(missing_auth);
    drop(forbidden);
    drop(sender);
    server.await.expect("H3 server task");
    finish_driver(driver).await;
}

#[tokio::test]
async fn malformed_connect_pseudo_headers_are_reset_with_h3_message_error() {
    let fixture = h3_fixture::endpoints();
    let server_endpoint = fixture.server;
    let server = tokio::spawn(async move {
        let connection = accept_server(&server_endpoint).await;
        let mut h3: h3::server::Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 server connection");

        let resolver = h3.accept().await.unwrap().unwrap();
        let (request, mut stream) = resolver
            .resolve_request()
            .await
            .expect("decoder exposes malformed CONNECT shape");
        let error = support::decide(Protocol::Http3, client_addr(), &request)
            .await
            .expect_err("scheme/path CONNECT rejection");
        assert!(matches!(
            error,
            DecisionError::InvalidHttp3(H3RequestError::MalformedClassicConnect)
        ));
        stream.stop_stream(Code::H3_MESSAGE_ERROR);
        stream.stop_sending(Code::H3_MESSAGE_ERROR);

        let resolver = h3.accept().await.unwrap().unwrap();
        let Err(error) = resolver.resolve_request().await else {
            panic!("missing authority was accepted");
        };
        assert!(matches!(
            error,
            h3::error::StreamError::StreamError {
                code: Code::H3_MESSAGE_ERROR,
                ..
            }
        ));
        let _ = h3.accept().await;
    });

    let connection = connect_client(&fixture.client, fixture.server_address).await;
    let raw_connection = connection.clone();
    let (driver, sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);

    let stopped = send_raw_headers(
        &raw_connection,
        &[
            (b":method", b"CONNECT"),
            (b":scheme", b"https"),
            (b":authority", b"example.com:443"),
            (b":path", b"/"),
        ],
    )
    .await;
    assert_eq!(stopped, Code::H3_MESSAGE_ERROR.value());
    let stopped = send_raw_headers(&raw_connection, &[(b":method", b"CONNECT")]).await;
    assert_eq!(stopped, Code::H3_MESSAGE_ERROR.value());

    drop(sender);
    server.await.expect("H3 server task");
    finish_driver(driver).await;
}

#[tokio::test]
async fn cancelling_classic_connect_resets_the_relay_and_releases_the_upstream() {
    let fixture = h3_fixture::endpoints();
    let server_endpoint = fixture.server;
    let (outcome_tx, outcome_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let connection = accept_server(&server_endpoint).await;
        let mut h3: h3::server::Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("H3 server connection");
        let resolver = h3.accept().await.unwrap().unwrap();
        let (request, mut stream) = resolver.resolve_request().await.expect("CONNECT request");
        support::decide(Protocol::Http3, client_addr(), &request)
            .await
            .expect("CONNECT decision");
        stream
            .send_response(success_response())
            .await
            .expect("CONNECT response");
        let (proxy_upstream, mut origin) = tokio::io::duplex(64);
        let origin_task = tokio::spawn(async move {
            let mut discarded = Vec::new();
            origin
                .read_to_end(&mut discarded)
                .await
                .expect("upstream release");
        });
        let outcome = tunnel(64).relay_h3(stream, proxy_upstream).await;
        origin_task.await.expect("origin task");
        outcome_tx.send(outcome).ok();
        let _ = h3.accept().await;
    });

    let connection = connect_client(&fixture.client, fixture.server_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut stream = sender
        .send_request(connect_request("example.com:443", true))
        .await
        .expect("CONNECT request");
    assert_eq!(
        stream
            .recv_response()
            .await
            .expect("CONNECT response")
            .status(),
        StatusCode::OK
    );
    stream.stop_stream(Code::H3_REQUEST_CANCELLED);
    stream.stop_sending(Code::H3_REQUEST_CANCELLED);
    assert!(matches!(
        timeout(Duration::from_secs(2), outcome_rx)
            .await
            .expect("cancel timeout")
            .expect("cancel outcome"),
        TunnelOutcome::Io { .. }
    ));
    drop(stream);
    drop(sender);
    server.await.expect("H3 server task");
    finish_driver(driver).await;
}

#[tokio::test]
async fn quic_rejects_connections_without_h3_alpn() {
    let fixture = h3_fixture::endpoints_with_alpn(vec![H3_ALPN.to_vec()], vec![b"not-h3".to_vec()]);
    let server_endpoint = fixture.server;
    let server = tokio::spawn(async move {
        server_endpoint
            .accept()
            .await
            .expect("incoming QUIC")
            .await
            .expect_err("server ALPN rejection")
    });
    fixture
        .client
        .connect(fixture.server_address, "localhost")
        .expect("start QUIC connection")
        .await
        .expect_err("client ALPN rejection");
    server.await.expect("server task");
}

fn connect_request(authority: &str, authenticated: bool) -> Request<()> {
    let mut builder = Request::builder().method(Method::CONNECT).uri(authority);
    if authenticated {
        builder = builder.header("proxy-authorization", "Bearer wire-test");
    }
    builder.body(()).expect("classic CONNECT request")
}

fn client_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 44_321))
}

fn success_response() -> Response<()> {
    Response::builder()
        .status(StatusCode::OK)
        .body(())
        .expect("CONNECT response")
}

fn rejection_response(error: &DecisionError) -> Response<()> {
    let rejection = error.rejection();
    let mut response = Response::new(());
    *response.status_mut() = rejection.status;
    if rejection.proxy_authenticate {
        response.headers_mut().insert(
            http::header::PROXY_AUTHENTICATE,
            http::HeaderValue::from_static("Bearer realm=\"forward-proxy\""),
        );
    }
    response
}

fn tunnel(max_bytes_per_direction: u64) -> BoundedTunnel {
    BoundedTunnel::new(TunnelLimits {
        max_bytes_per_direction,
        idle_timeout: Duration::from_secs(2),
        lifetime_timeout: Duration::from_secs(5),
        buffer_size: 16,
    })
    .expect("tunnel limits")
}

async fn connect_client(endpoint: &quinn::Endpoint, address: SocketAddr) -> quinn::Connection {
    endpoint
        .connect(address, "localhost")
        .expect("start QUIC connection")
        .await
        .expect("QUIC client handshake")
}

async fn accept_server(endpoint: &quinn::Endpoint) -> quinn::Connection {
    endpoint
        .accept()
        .await
        .expect("incoming QUIC connection")
        .await
        .expect("QUIC server handshake")
}

fn assert_h3_alpn(connection: &quinn::Connection) {
    let handshake = connection
        .handshake_data()
        .expect("handshake data")
        .downcast::<HandshakeData>()
        .expect("rustls handshake data");
    assert_eq!(handshake.protocol.as_deref(), Some(H3_ALPN));
}

fn drive_client<C>(mut driver: h3::client::Connection<C, Bytes>) -> tokio::task::JoinHandle<()>
where
    C: h3::quic::Connection<Bytes> + Send + 'static,
    C::SendStream: Send,
    C::RecvStream: Send,
{
    tokio::spawn(async move {
        let _ = poll_fn(|context| driver.poll_close(context)).await;
    })
}

async fn finish_driver(driver: tokio::task::JoinHandle<()>) {
    assert!(
        timeout(Duration::from_secs(2), driver).await.is_ok(),
        "H3 driver did not stop"
    );
}

async fn recv_chunk<S>(stream: &mut h3::client::RequestStream<S, Bytes>) -> Bytes
where
    S: h3::quic::RecvStream,
{
    let mut chunk = stream
        .recv_data()
        .await
        .expect("tunnel response DATA")
        .expect("tunnel response chunk");
    chunk.copy_to_bytes(chunk.remaining())
}

async fn send_raw_headers(
    connection: &quinn::Connection,
    fields: &[(&'static [u8], &'static [u8])],
) -> u64 {
    let fields = fields
        .iter()
        .map(|(name, value)| HeaderField::from((*name, *value)))
        .collect::<Vec<_>>();
    let mut block = BytesMut::new();
    qpack::encode_stateless(&mut block, fields).expect("QPACK request block");
    let mut frame = BytesMut::new();
    frame.write_var(0x1);
    frame.write_var(block.len() as u64);
    frame.extend_from_slice(&block);
    let (mut send, _recv) = connection.open_bi().await.expect("raw request stream");
    send.write_all(&frame).await.expect("raw HEADERS frame");
    send.stopped()
        .await
        .expect("server stopped malformed request")
        .expect("malformed request stop code")
        .into_inner()
}
