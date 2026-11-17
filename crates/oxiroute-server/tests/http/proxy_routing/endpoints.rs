use crate::TEST_TIMEOUT;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::builders::endpoint_pool;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::assert_origin_response;
use crate::support::wire::read_request_head_from;
use oxiroute_config::HttpProxyPathRewrite;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::UpstreamAlgorithm;
use oxiroute_config::UpstreamEndpoint;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[tokio::test]
async fn connects_to_a_dns_http_endpoint_and_keeps_its_identity() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("dns", 1).await;
        let proxy = ProxyHarness::start(
            vec![endpoint_pool(
                "dns",
                vec![UpstreamEndpoint::Dns {
                    host: "localhost".into(),
                    port: origin.address.port(),
                }],
                UpstreamAlgorithm::RoundRobin,
            )],
            vec![route(None, "/", &[], "dns")],
            1024,
            1,
        )
        .await;

        let response = proxy.request("GET / HTTP/1.1\r\nHost: dns.test\r\n").await;
        assert_origin_response(&response, "dns");
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0]
                .address
                .to_string(),
            format!("localhost:{}", origin.address.port())
        );

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("DNS HTTP test timed out");
}

#[cfg(unix)]
#[tokio::test]
async fn connects_to_a_unix_http_endpoint() {
    use tokio::net::UnixListener;

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("Unix HTTP directory");
        let path = directory.path().join("origin.sock");
        let listener = UnixListener::bind(&path).expect("Unix HTTP bind");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("Unix HTTP accept");
            read_request_head_from(&mut stream)
                .await
                .expect("Unix HTTP request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nunix")
                .await
                .expect("Unix HTTP response");
        });
        let proxy = ProxyHarness::start(
            vec![endpoint_pool(
                "unix",
                vec![UpstreamEndpoint::Unix { path: path.clone() }],
                UpstreamAlgorithm::RoundRobin,
            )],
            vec![route(None, "/", &[], "unix")],
            1024,
            1,
        )
        .await;

        let response = proxy.request("GET / HTTP/1.1\r\nHost: unix.test\r\n").await;
        assert_origin_response(&response, "unix");

        proxy.finish().await;
        origin.await.expect("Unix HTTP origin task");
    })
    .await
    .expect("Unix HTTP test timed out");
}

#[tokio::test]
async fn proxy_path_replacement_rewrites_the_origin_path_and_preserves_the_query() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("path rewrite origin bind");
        let origin_address = listener.local_addr().expect("path rewrite origin address");
        let (seen_tx, seen_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("path rewrite origin accept");
            let request = read_request_head_bytes(&mut stream)
                .await
                .expect("path rewrite origin request");
            seen_tx
                .send(String::from_utf8(request).expect("path rewrite request UTF-8"))
                .expect("path rewrite request observation");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("path rewrite origin response");
        });
        let mut proxy_route = route(None, "/api/", &[], "rewrite");
        let HttpRouteAction::Proxy { policy, .. } = &mut proxy_route.action else {
            unreachable!();
        };
        policy.upstream_path_rewrite = Some(HttpProxyPathRewrite {
            from: "/api/".into(),
            to: "/v1/".into(),
        });
        let proxy = ProxyHarness::start(
            vec![pool("rewrite", &[origin_address])],
            vec![proxy_route],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request("GET /api/items?id=7 HTTP/1.1\r\nHost: rewrite.test\r\n")
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.body(), b"ok");
        let request = seen_rx.await.expect("path rewrite request");
        assert!(
            request.starts_with("GET /v1/items?id=7 HTTP/1.1\r\n"),
            "origin request: {request}"
        );

        proxy.finish().await;
        origin.await.expect("path rewrite origin task");
    })
    .await
    .expect("path rewrite test timed out");
}
