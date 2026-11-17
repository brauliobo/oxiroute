use crate::TEST_TIMEOUT;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::wire::read_request_head;
use oxiroute_config::DownstreamTimeoutPolicy;
use oxiroute_config::HttpGzipMinimumVersion;
use oxiroute_config::HttpGzipPolicy;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one origin sequence covers gzip policy and negotiation on the wire"
)]
async fn configured_gzip_honors_exact_types_defaults_and_accept_encoding() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("gzip origin bind");
        let origin_address = listener.local_addr().expect("gzip origin address");
        let origin = tokio::spawn(async move {
            for (content_type, body_len) in [
                ("application/json", 512),
                ("image/custom", 512),
                ("text/plain", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 512),
                ("application/json", 19),
            ] {
                let (mut stream, _) = listener.accept().await.expect("gzip origin accept");
                read_request_head(&mut stream).await.expect("gzip request");
                let body = vec![b'a'; body_len];
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.expect("gzip head");
                stream.write_all(&body).await.expect("gzip body");
            }
        });
        let proxy = ProxyHarness::start_with_features(
            vec![pool("origin", &[origin_address])],
            vec![route(None, "/", &[], "origin")],
            Some(1024),
            true,
            100,
            0,
            false,
            14,
            Some(HttpGzipPolicy {
                level: 6,
                content_types: vec!["application/json".into(), "image/custom".into()],
                min_length_bytes: 20,
                min_http_version: HttpGzipMinimumVersion::Http11,
                disable_on_via: true,
                vary: true,
            }),
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        let compressed = proxy
            .request("GET /json HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(compressed.header("content-encoding"), Some("gzip"));
        assert_eq!(compressed.body().get(..2), Some(&[0x1f, 0x8b][..]));
        assert!(compressed.body().len() < 512);
        assert_eq!(compressed.header("vary"), Some("accept-encoding"));

        let concrete_type = proxy
            .request("GET /image HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(concrete_type.header("content-encoding"), Some("gzip"));

        let plain = proxy
            .request("GET /text HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(plain.header("content-encoding"), None);
        assert_eq!(plain.body().len(), 512);

        let gzip_not_brotli = proxy
            .request("GET /preferred HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: br, gzip;q=0.5\r\n")
            .await;
        assert_eq!(gzip_not_brotli.header("content-encoding"), Some("gzip"));

        let rejected = proxy
            .request("GET /rejected HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip;q=0\r\n")
            .await;
        assert_eq!(rejected.header("content-encoding"), None);

        let wildcard = proxy
            .request("GET /wildcard HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: *\r\n")
            .await;
        assert_eq!(wildcard.header("content-encoding"), Some("gzip"));

        let explicit_rejection = proxy
            .request("GET /override HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: *;q=1, gzip;q=0\r\n")
            .await;
        assert_eq!(explicit_rejection.header("content-encoding"), None);

        let explicit_acceptance = proxy
            .request("GET /override HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: *;q=0, gzip;q=0.5\r\n")
            .await;
        assert_eq!(explicit_acceptance.header("content-encoding"), Some("gzip"));

        let repeated_rejection = proxy
            .request("GET /repeated HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: *;q=1\r\nAccept-Encoding: gzip;q=0\r\n")
            .await;
        assert_eq!(repeated_rejection.header("content-encoding"), None);

        let repeated_acceptance = proxy
            .request("GET /repeated HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: *;q=0\r\nAccept-Encoding: gzip;q=0.5\r\n")
            .await;
        assert_eq!(repeated_acceptance.header("content-encoding"), Some("gzip"));

        let absent = proxy
            .request("GET /absent HTTP/1.1\r\nHost: gzip.test\r\n")
            .await;
        assert_eq!(absent.header("content-encoding"), None);

        let old_http = proxy
            .request("GET /old HTTP/1.0\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(old_http.header("content-encoding"), None);
        assert_eq!(old_http.header("vary"), Some("accept-encoding"));

        let via = proxy
            .request("GET /via HTTP/1.1\r\nHost: gzip.test\r\nVia: 1.1 proxy\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(via.header("content-encoding"), None);
        assert_eq!(via.header("vary"), Some("accept-encoding"));

        let short = proxy
            .request("GET /short HTTP/1.1\r\nHost: gzip.test\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(short.header("content-encoding"), None);
        assert_eq!(short.body().len(), 19);

        proxy.finish().await;
        origin.await.expect("gzip origin task");
    })
    .await
    .expect("gzip policy test timed out");
}

#[tokio::test]
async fn persisted_gzip_defaults_allow_http_1_0_via_and_emit_vary() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("gzip origin bind");
        let origin_address = listener.local_addr().expect("gzip origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("gzip origin accept");
            read_request_head(&mut stream).await.expect("gzip request");
            let body = vec![b'a'; 512];
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.expect("gzip head");
            stream.write_all(&body).await.expect("gzip body");
        });
        let proxy = ProxyHarness::start_with_features(
            vec![pool("origin", &[origin_address])],
            vec![route(None, "/", &[], "origin")],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            Some(HttpGzipPolicy {
                level: 6,
                content_types: vec!["application/json".into()],
                ..HttpGzipPolicy::default()
            }),
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.0\r\nHost: gzip.test\r\nVia: 1.0 legacy\r\nAccept-Encoding: gzip\r\n")
            .await;
        assert_eq!(response.header("content-encoding"), Some("gzip"));
        assert_eq!(response.header("vary"), Some("accept-encoding"));

        proxy.finish().await;
        origin.await.expect("gzip origin task");
    })
    .await
    .expect("persisted gzip defaults test timed out");
}
