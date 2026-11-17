use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use oxiroute_config::{
    AlpnProtocol, HttpRetryPolicy, HttpRetryTrigger, HttpRouteAction, HttpVersion,
    HttpVersionPolicy, UpstreamServer,
};
use tokio::time::{sleep, timeout};

use crate::support::{
    GRPC_BODY, H2Client, ORIGIN_SERVER_NAME, PROXY_SERVER_NAME, ProxyHarness, ReservedListener,
    TEST_TIMEOUT, TlsOrigin, negotiated_alpn, proxy_config, socket_endpoint, tls_connect,
    verified_upstream,
};

#[tokio::test]
async fn h2_only_upstream_preserves_grpc_data_and_trailers_without_h1_downgrade() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(
            proxy.address,
            PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"h2", b"http/1.1"],
        )
        .await
        .expect("H2 downstream connection");
        assert_eq!(negotiated_alpn(&stream), Some(b"h2".as_slice()));
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 client handshake");

        let h2 = client
            .request(Method::GET, "/h2")
            .await
            .expect("H2-only upstream response");
        assert_eq!(h2.status, StatusCode::OK);
        assert_eq!(h2.body, b"h2-origin".as_slice());

        let grpc = client
            .request(Method::POST, "/grpc/full")
            .await
            .expect("full gRPC response");
        assert_eq!(grpc.status, StatusCode::OK);
        assert_eq!(grpc.headers["content-type"], "application/grpc");
        assert_eq!(grpc.body, GRPC_BODY);
        let trailers = grpc.trailers.expect("full gRPC trailers");
        assert_eq!(trailers["grpc-status"], "0");
        assert_eq!(trailers["grpc-message"], "completed");
        assert_eq!(trailers["x-oxiroute-trailer"], "full");

        let trailers_only = client
            .request(Method::POST, "/grpc/error")
            .await
            .expect("trailers-only gRPC response");
        assert_eq!(trailers_only.status, StatusCode::OK);
        assert_eq!(trailers_only.headers["content-type"], "application/grpc");
        assert!(trailers_only.body.is_empty());
        let trailers = trailers_only.trailers.expect("trailers-only metadata");
        assert_eq!(trailers["grpc-status"], "7");
        assert_eq!(trailers["grpc-message"], "permission denied");
        assert_eq!(trailers["x-oxiroute-trailer"], "trailers-only");

        origin.observations.wait_for_http_requests(3).await;
        assert_eq!(origin.observations.http_requests(), 3);
        assert_eq!(
            origin.observations.server_names(),
            vec![ORIGIN_SERVER_NAME.to_owned()]
        );
        assert_eq!(origin.observations.alpn(), vec![b"h2".to_vec()]);

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream H2/gRPC wire test timed out");
}

#[tokio::test]
async fn h2_grpc_streaming_data_and_trailers_survive_without_downgrade() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("streaming gRPC downstream connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("streaming gRPC H2 client");

        let response = client
            .request(Method::POST, "/grpc/stream")
            .await
            .expect("streaming gRPC response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.headers[header::CONTENT_TYPE], "application/grpc");
        assert_eq!(
            response.body,
            Bytes::from_static(b"\0\0\0\0\x05hello\0\0\0\0\x05world\0\0\0\0\x05again")
        );
        let trailers = response.trailers.expect("streaming gRPC trailers");
        assert_eq!(trailers["grpc-status"], "0");
        assert_eq!(trailers["grpc-message"], "streamed");
        assert_eq!(trailers["x-oxiroute-trailer"], "stream");
        origin.observations.wait_for_http_requests(1).await;

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("streaming H2/gRPC wire test timed out");
}

#[tokio::test]
async fn h2_grpc_request_data_streams_to_an_h2_origin() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("request-streaming H2 connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("request-streaming H2 client");
        let body = vec![
            Bytes::from_static(b"\0\0\0\0\x05hello"),
            Bytes::from_static(b"\0\0\0\0\x05world"),
            Bytes::from_static(b"\0\0\0\0\x05again"),
        ];
        let expected_body_bytes = body.iter().map(Bytes::len).sum::<usize>();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/grpc"),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&expected_body_bytes.to_string()).expect("body length header"),
        );
        let response = client
            .request_with_headers_and_body_chunks(Method::POST, "/grpc/upload", headers, body)
            .await
            .expect("request-streaming gRPC response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, GRPC_BODY);
        assert_eq!(
            origin.observations.request_body_bytes(),
            expected_body_bytes
        );
        let trailers = response.trailers.expect("request-streaming trailers");
        assert_eq!(trailers["grpc-status"], "0");
        assert_eq!(trailers["x-oxiroute-trailer"], "upload");

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("request-streaming H2/gRPC wire test timed out");
}

#[tokio::test]
async fn downstream_h2_cancellation_resets_the_h2_origin_stream() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("cancellation H2 connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("cancellation H2 client");

        client
            .cancel_request(
                Method::POST,
                "/grpc/slow",
                origin.observations.wait_for_http_requests(1),
            )
            .await
            .expect("downstream cancellation reset");
        timeout(TEST_TIMEOUT, async {
            while origin.observations.h2_send_errors() == 0 {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("upstream H2 stream was not reset after downstream cancellation");

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("H2 cancellation wire test timed out");
}

#[tokio::test]
async fn downstream_h2_deadline_returns_bounded_upstream_error_and_resets_origin() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        config.http_services[0].routes[0].policy.read_timeout_ms = 50;
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("deadline H2 connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("deadline H2 client");

        let response = client
            .request(Method::POST, "/grpc/slow")
            .await
            .expect("deadline error response");
        assert_eq!(response.status, StatusCode::GATEWAY_TIMEOUT);
        origin.observations.wait_for_http_requests(1).await;

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("H2 deadline wire test timed out");
}

#[tokio::test]
async fn downstream_h2_large_bounded_body_returns_413_before_origin_admission() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        config.http_services[0].routes[0]
            .policy
            .max_request_body_bytes = Some(8);
        config.http_services[0].routes[0].policy.request_buffering = true;
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("bounded-body H2 connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("bounded-body H2 client");

        let response = client
            .request_with_body_chunks(
                Method::POST,
                "/grpc/upload",
                vec![Bytes::from_static(b"123456789")],
            )
            .await
            .expect("bounded-body response");
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(response.body.is_empty());
        sleep(Duration::from_millis(25)).await;
        assert_eq!(origin.observations.http_requests(), 0);

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("bounded-body H2 wire test timed out");
}

#[tokio::test]
async fn upstream_h2_reset_and_malformed_flow_fail_closed_without_downgrade() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("reset H2 connection");
        let mut client = H2Client::from_tls(stream).await.expect("reset H2 client");

        let reset = client
            .request(Method::POST, "/grpc/reset")
            .await
            .expect("reset response");
        assert_eq!(reset.status, StatusCode::BAD_GATEWAY);
        let malformed = client
            .request(Method::POST, "/grpc/malformed")
            .await
            .expect("malformed-flow response");
        assert_eq!(malformed.status, StatusCode::BAD_GATEWAY);
        assert!(malformed.body.is_empty());
        origin.observations.wait_for_http_requests(2).await;

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream H2 reset/malformed wire test timed out");
}

#[tokio::test]
async fn downstream_h2_flow_control_backpressures_a_large_grpc_stream() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("flow-control H2 connection");
        let mut builder = h2::client::Builder::new();
        builder
            .initial_window_size(1_024)
            .initial_connection_window_size(1_024);
        let (sender, connection) = builder
            .handshake::<_, Bytes>(stream)
            .await
            .expect("H2 handshake");
        let driver = tokio::spawn(connection);
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("https://{PROXY_SERVER_NAME}/grpc/large"))
            .body(())
            .expect("large-stream request");
        let (response, _request_body) = sender
            .ready()
            .await
            .expect("large-stream sender readiness")
            .send_request(request, true)
            .expect("large-stream request");
        let response = timeout(TEST_TIMEOUT, response)
            .await
            .expect("large-stream response headers timeout")
            .expect("large-stream response headers");
        assert_eq!(response.status(), StatusCode::OK);
        sleep(Duration::from_millis(25)).await;

        let mut body = response.into_body();
        let mut received = 0usize;
        while let Some(chunk) = body.data().await {
            let chunk = chunk.expect("large-stream DATA");
            received += chunk.len();
            body.flow_control()
                .release_capacity(chunk.len())
                .expect("large-stream flow-control release");
        }
        let trailers = body.trailers().await.expect("large-stream trailers");
        assert_eq!(received, 256 * 16 * 1024);
        assert_eq!(
            trailers.expect("large-stream trailing metadata")["grpc-status"],
            "0"
        );
        origin.observations.wait_for_http_requests(1).await;

        driver.abort();
        let _ = driver.await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("H2 flow-control wire test timed out");
}

#[tokio::test]
async fn maxconn_one_multiplexes_concurrent_h2_requests_on_one_physical_connection() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            HttpVersionPolicy {
                min: HttpVersion::Http2,
                max: HttpVersion::Http2,
            },
        );
        let endpoint = config.upstream_pools[0]
            .endpoints
            .pop()
            .expect("upstream endpoint");
        config.upstream_pools[0].servers.push(UpstreamServer {
            name: "origin".into(),
            endpoint,
            max_connections: Some(1),
            dns_resolution: oxiroute_config::DnsResolutionPolicy::OnConnect,
        });
        config.upstream_pools[0].queue_timeout_ms = Some(1_000);
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let proxy_address = proxy.address;
        let request = |path: &'static str| async move {
            let stream = tls_connect(proxy_address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
                .await
                .expect("capped downstream H2 connection");
            let mut client = H2Client::from_tls(stream)
                .await
                .expect("capped downstream H2 client");
            let response = client
                .request(Method::GET, path)
                .await
                .expect("capped H2 response");
            client.finish().await;
            response
        };

        let (first, second) = tokio::join!(request("/h2"), request("/h2"));
        assert_eq!(first.body.as_ref(), b"h2-origin");
        assert_eq!(second.body.as_ref(), b"h2-origin");
        origin.observations.wait_for_http_requests(2).await;
        assert_eq!(origin.observations.alpn(), vec![b"h2".to_vec()]);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("capped H2 multiplexing test timed out");
}

#[tokio::test]
async fn h2_refused_stream_retries_a_bodyless_get_on_a_distinct_endpoint() {
    timeout(TEST_TIMEOUT, async {
        let refusing = TlsOrigin::start_h2_refusing_streams().await;
        let healthy = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let mut config = proxy_config(
            reserved.address,
            refusing.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        config.upstream_pools[0]
            .endpoints
            .push(socket_endpoint(healthy.address));
        let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
        else {
            panic!("wire route must proxy");
        };
        policy.retry = HttpRetryPolicy {
            max_retries: 1,
            triggers: vec![HttpRetryTrigger::RefusedStream],
            ..HttpRetryPolicy::default()
        };
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("downstream H2 retry TLS connect");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("downstream H2 retry client");

        let response = client
            .request(Method::GET, "/h2")
            .await
            .expect("retried H2 request");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"h2-origin".as_slice());
        refusing.observations.wait_for_http_requests(1).await;
        healthy.observations.wait_for_http_requests(1).await;

        client.finish().await;
        proxy.finish().await;
        refusing.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("H2 refused-stream retry test timed out");
}
