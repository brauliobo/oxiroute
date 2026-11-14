//! HTTP/3 to TLS origins: HTTP/1.1 and HTTP/2 over the connection the pool's TLS policy negotiates.
#[allow(clippy::wildcard_imports)]
use super::*;

fn forwarded_proto() -> HttpProxyPolicy {
    HttpProxyPolicy {
        request_headers: vec![HttpRequestHeaderMutation::Set {
            name: "x-forwarded-proto".into(),
            value: HttpRequestHeaderValue::DownstreamScheme,
        }],
        ..HttpProxyPolicy::default()
    }
}

async fn tls_daemon(origin_address: SocketAddr, min: HttpVersion, max: HttpVersion) -> Daemon {
    let listener = reserve_udp_address();
    let route = route("/", "origin", forwarded_proto());
    let config = config(
        listener,
        service(vec![route]),
        vec![tls_pool("origin", origin_address, min, max)],
    );
    Daemon::start(listener, &config).await
}

#[tokio::test]
pub(super) async fn h3_reaches_a_tls_h1_origin() {
    let origin = support::TlsOrigin::start_h1(b"tls-h1").await;
    let mut daemon = tls_daemon(origin.address, HttpVersion::Http11, HttpVersion::Http11).await;
    let answer = daemon.get("/secure").await;
    assert_eq!((answer.status, answer.text().as_str()), (200, "tls-h1"));
    let observed = &origin.observations;
    assert_eq!(observed.server_names(), [support::ORIGIN_SERVER_NAME]);
    assert_eq!(observed.alpn(), [b"http/1.1".to_vec()]);
    let head = String::from_utf8(observed.request_heads().remove(0)).expect("text head");
    assert!(head.starts_with("GET /secure HTTP/1.1\r\n"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("x-forwarded-proto: https\r\n"),
        "{head}"
    );
    daemon.finish().await;
    origin.finish().await;
}

#[tokio::test]
pub(super) async fn h3_reaches_a_tls_h2_origin_with_its_trailers() {
    let origin = support::TlsOrigin::start_h2().await;
    let mut daemon = tls_daemon(origin.address, HttpVersion::Http2, HttpVersion::Http2).await;

    let answer = daemon.get("/h2").await;
    assert_eq!((answer.status, answer.text().as_str()), (200, "h2-origin"));
    assert_eq!(answer.header("content-type"), Some("text/plain"));

    let answer = daemon.get("/grpc/full").await;
    assert_eq!(
        (answer.status, answer.body.as_ref()),
        (200, support::GRPC_BODY)
    );
    let trailers = answer.trailers.expect("origin trailers");
    assert_eq!(
        trailers.get("grpc-status").map(http::HeaderValue::as_bytes),
        Some(&b"0"[..])
    );
    assert_eq!(
        trailers
            .get("x-oxiroute-trailer")
            .map(http::HeaderValue::as_bytes),
        Some(&b"full"[..])
    );

    let answer = daemon
        .send(
            "POST",
            "/grpc/upload",
            &[("content-type", "application/grpc")],
            support::GRPC_BODY,
        )
        .await;
    assert_eq!(answer.status, 200);
    assert_eq!(
        origin.observations.request_body_bytes(),
        support::GRPC_BODY.len()
    );

    assert_eq!(answer.header("content-type"), Some("application/grpc"));
    assert!(origin.observations.alpn().iter().all(|alpn| alpn == b"h2"));
    let headers = origin.observations.h2_request_headers();
    assert_eq!(
        headers[0]
            .get("x-forwarded-proto")
            .map(http::HeaderValue::as_bytes),
        Some(&b"https"[..])
    );
    assert_eq!(
        origin.observations.accepted(),
        1,
        "one HTTP/2 connection carries every stream"
    );
    daemon.finish().await;
    origin.finish().await;
}

#[tokio::test]
pub(super) async fn h3_to_an_h2_origin_that_resets_answers_bad_gateway() {
    let origin = support::TlsOrigin::start_h2().await;
    let mut daemon = tls_daemon(origin.address, HttpVersion::Http2, HttpVersion::Http2).await;
    assert_eq!(daemon.get("/grpc/reset").await.status, 502);
    assert_eq!(daemon.get("/grpc/malformed").await.status, 502);
    assert_eq!(
        daemon.get("/h2").await.status,
        200,
        "the origin still answers after the failed streams"
    );
    daemon.finish().await;
    origin.finish().await;
}

#[tokio::test]
pub(super) async fn h3_to_a_tls_origin_whose_certificate_the_pool_does_not_trust_answers_bad_gateway()
 {
    let origin = support::TlsOrigin::start_h1(b"never").await;
    let listener = reserve_udp_address();
    let mut untrusting = tls_pool(
        "origin",
        origin.address,
        HttpVersion::Http11,
        HttpVersion::Http11,
    );
    untrusting.tls.as_mut().expect("pool TLS").server_name = Some("other.example.test".into());
    let config = config(
        listener,
        service(vec![route("/", "origin", HttpProxyPolicy::default())]),
        vec![untrusting],
    );
    let mut daemon = Daemon::start(listener, &config).await;
    assert_eq!(daemon.get("/").await.status, 502);
    assert_eq!(origin.observations.http_requests(), 0);
    daemon.finish().await;
    origin.finish().await;
}
