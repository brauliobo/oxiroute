use std::{
    fs::File,
    io::BufReader,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use h3::server::Connection;
use http::{HeaderMap, Method, Request, Response, StatusCode};
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::PrivateKeyDer;
use tokio::sync::watch;

use super::*;

const ORIGIN_SERVER_NAME: &str = "origin.example.test";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(option_env!("OXIROUTE_SERVER_MANIFEST_DIR").unwrap_or(env!("CARGO_MANIFEST_DIR")))
        .join("tests/fixtures")
        .join(name)
}

fn test_plan(max_connections: usize) -> H3UpstreamPlan {
    let roots = custom_roots("HTTP/3 upstream test", &fixture("ca-a.pem")).expect("test CA bundle");
    H3UpstreamPlan::new(
        "HTTP/3 upstream test".into(),
        roots,
        Some(ORIGIN_SERVER_NAME.into()),
        max_connections,
    )
    .expect("HTTP/3 client plan")
}

fn origin_server_config(alpn: &[u8]) -> quinn::ServerConfig {
    let mut certificate_reader =
        BufReader::new(File::open(fixture("origin.pem")).expect("origin certificate fixture"));
    let certificates = CertificateDer::pem_reader_iter(&mut certificate_reader)
        .collect::<Result<Vec<_>, _>>()
        .expect("origin certificate chain");
    let mut key_reader =
        BufReader::new(File::open(fixture("origin-key.pem")).expect("origin private-key fixture"));
    let private_key = PrivateKeyDer::pem_reader_iter(&mut key_reader)
        .next()
        .transpose()
        .expect("origin private key")
        .expect("origin private key block");
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .expect("origin TLS identity");
    crypto.alpn_protocols = vec![alpn.to_vec()];
    crypto.max_early_data_size = 0;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).expect("QUIC TLS configuration"),
    ));
    config.migration(false);
    config
}

#[tokio::test]
async fn round_trips_h3_request_body_response_and_trailers() {
    let endpoint =
        quinn::Endpoint::server(origin_server_config(b"h3"), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("origin endpoint");
    let address = endpoint.local_addr().expect("origin address");
    let origin = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("origin incoming connection");
        let connection = incoming.await.expect("origin QUIC connection");
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("origin H3 accept")
            .expect("origin H3 request");
        let (request, mut stream) = resolver.resolve_request().await.expect("origin request");
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.uri().path(), "/h3");
        assert_eq!(
            request.uri().authority().map(http::uri::Authority::as_str),
            Some(ORIGIN_SERVER_NAME)
        );
        let mut body = Vec::new();
        while let Some(mut chunk) = stream.recv_data().await.expect("origin request body") {
            let length = chunk.remaining();
            body.extend_from_slice(&chunk.copy_to_bytes(length));
        }
        assert_eq!(body, b"request-body");
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("x-origin", "h3")
                    .body(())
                    .expect("origin response"),
            )
            .await
            .expect("origin response headers");
        stream
            .send_data(Bytes::from_static(b"response-body"))
            .await
            .expect("origin response body");
        let mut trailers = HeaderMap::new();
        trailers.insert(
            "x-origin-trailer",
            "complete".parse().expect("trailer value"),
        );
        stream
            .send_trailers(trailers)
            .await
            .expect("origin response trailers");
        stream.finish().await.expect("origin response finish");
        let _ = h3.accept().await;
    });

    let plan = test_plan(1);
    let request = Request::builder()
        .method(Method::POST)
        .uri("https://origin.example.test/h3")
        .body(())
        .expect("upstream request");
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let response = plan
        .request(
            address,
            ORIGIN_SERVER_NAME,
            request,
            Bytes::from_static(b"request-body"),
            Instant::now() + Duration::from_secs(5),
            shutdown,
        )
        .await;
    origin.await.expect("origin task");
    let response = response.expect("H3 upstream response");
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers["x-origin"], "h3");
    assert_eq!(response.body, Bytes::from_static(b"response-body"));
    assert_eq!(
        response.trailers.expect("response trailers")["x-origin-trailer"],
        "complete"
    );
    assert_eq!(plan.connections.available_permits(), 1);
}

#[tokio::test]
async fn rejects_a_mismatched_upstream_content_length() {
    let endpoint =
        quinn::Endpoint::server(origin_server_config(b"h3"), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("origin endpoint");
    let address = endpoint.local_addr().expect("origin address");
    let origin = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("origin incoming connection");
        let connection = incoming.await.expect("origin QUIC connection");
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("origin H3 accept")
            .expect("origin H3 request");
        let (_request, mut stream) = resolver.resolve_request().await.expect("origin request");
        assert!(
            stream
                .recv_data()
                .await
                .expect("origin request body")
                .is_none()
        );
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", "4")
                    .body(())
                    .expect("origin response"),
            )
            .await
            .expect("origin response headers");
        stream
            .send_data(Bytes::from_static(b"bad"))
            .await
            .expect("origin response body");
        stream.finish().await.expect("origin response finish");
        let _ = h3.accept().await;
    });

    let plan = test_plan(1);
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let error = plan
        .request(
            address,
            ORIGIN_SERVER_NAME,
            Request::builder()
                .method(Method::GET)
                .uri("https://origin.example.test/mismatch")
                .body(())
                .expect("upstream request"),
            Bytes::new(),
            Instant::now() + Duration::from_secs(5),
            shutdown,
        )
        .await
        .expect_err("mismatched response length");
    assert!(matches!(error, H3UpstreamError::Protocol));
    origin.await.expect("origin task");
}

#[tokio::test]
async fn preserves_head_response_content_length_without_a_body() {
    let endpoint =
        quinn::Endpoint::server(origin_server_config(b"h3"), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("origin endpoint");
    let address = endpoint.local_addr().expect("origin address");
    let origin = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("origin incoming connection");
        let connection = incoming.await.expect("origin QUIC connection");
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("origin H3 accept")
            .expect("origin H3 request");
        let (_request, mut stream) = resolver.resolve_request().await.expect("origin request");
        assert!(
            stream
                .recv_data()
                .await
                .expect("origin request body")
                .is_none()
        );
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", "7")
                    .body(())
                    .expect("origin response"),
            )
            .await
            .expect("origin response headers");
        stream.finish().await.expect("origin response finish");
        let _ = h3.accept().await;
    });

    let plan = test_plan(1);
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let response = plan
        .request(
            address,
            ORIGIN_SERVER_NAME,
            Request::builder()
                .method(Method::HEAD)
                .uri("https://origin.example.test/head")
                .body(())
                .expect("upstream request"),
            Bytes::new(),
            Instant::now() + Duration::from_secs(5),
            shutdown,
        )
        .await
        .expect("HEAD upstream response");
    assert_eq!(response.headers[CONTENT_LENGTH], "7");
    assert!(response.body.is_empty());
    origin.await.expect("origin task");
}

#[tokio::test]
async fn rejects_an_upstream_that_does_not_negotiate_h3() {
    let endpoint = quinn::Endpoint::server(
        origin_server_config(b"http/1.1"),
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .expect("origin endpoint");
    let address = endpoint.local_addr().expect("origin address");
    let origin = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("origin incoming connection");
        assert!(incoming.await.is_err(), "ALPN mismatch must reject QUIC");
    });
    let plan = test_plan(1);
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let error = plan
        .request(
            address,
            ORIGIN_SERVER_NAME,
            Request::new(()),
            Bytes::new(),
            Instant::now() + Duration::from_secs(5),
            shutdown,
        )
        .await
        .expect_err("non-H3 origin must be rejected");
    assert!(matches!(
        error,
        H3UpstreamError::Connect | H3UpstreamError::Protocol
    ));
    origin.await.expect("origin task");
}

#[tokio::test]
async fn rejects_an_oversized_request_before_connection_admission() {
    let plan = test_plan(1);
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let error = plan
        .request(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
            ORIGIN_SERVER_NAME,
            Request::new(()),
            Bytes::from(vec![0; H3_UPSTREAM_MAX_REQUEST_BODY_BYTES + 1]),
            Instant::now() + Duration::from_secs(1),
            shutdown,
        )
        .await
        .expect_err("oversized request body");
    assert!(matches!(error, H3UpstreamError::RequestBodyTooLarge));
    assert_eq!(plan.connections.available_permits(), 1);
}

#[tokio::test]
async fn cancellation_interrupts_upstream_connection_admission() {
    let plan = test_plan(1);
    let (shutdown_sender, shutdown) = watch::channel(false);
    shutdown_sender.send(true).expect("shutdown watcher");
    let error = plan
        .request(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
            ORIGIN_SERVER_NAME,
            Request::new(()),
            Bytes::new(),
            Instant::now() + Duration::from_secs(1),
            shutdown,
        )
        .await
        .expect_err("cancelled upstream request");
    assert!(matches!(error, H3UpstreamError::Cancelled));
}

#[tokio::test]
async fn timeout_interrupts_upstream_connection_admission() {
    let plan = test_plan(1);
    let permit = plan
        .connections
        .clone()
        .try_acquire_owned()
        .expect("connection permit");
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let error = plan
        .request(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
            ORIGIN_SERVER_NAME,
            Request::new(()),
            Bytes::new(),
            Instant::now() + Duration::from_millis(1),
            shutdown,
        )
        .await
        .expect_err("timed out upstream request");
    drop(permit);
    assert!(matches!(error, H3UpstreamError::Timeout));
}
