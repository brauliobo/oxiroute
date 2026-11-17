use crate::TEST_TIMEOUT;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::read_request_head;
use oxiroute_config::DownstreamTimeoutPolicy;
use oxiroute_config::HttpCookieAttributePolicy;
use oxiroute_config::HttpCookiePathRewrite;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpProxyPolicy;
use oxiroute_config::HttpRequestHeaderMutation;
use oxiroute_config::HttpRequestHeaderValue;
use oxiroute_config::HttpResponseHeaderMutation;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::HttpSameSite;
use oxiroute_config::HttpUpstreamHost;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

#[tokio::test]
async fn applies_host_header_cookie_and_request_response_header_policies() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("policy origin bind");
        let origin_address = listener.local_addr().expect("policy origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("policy origin accept");
            let request = read_request_head_bytes(&mut stream)
                .await
                .expect("policy origin request");
            let request = String::from_utf8(request).expect("policy request UTF-8");
            assert!(request.contains("\r\nHost: selected.example:9443\r\n"));
            assert!(request.contains("\r\nx-incoming: Client.Example.:8080\r\n"));
            assert!(request.contains("\r\nx-normalized: client.example.\r\n"));
            assert!(request.contains("\r\nx-nginx-host: client.example\r\n"));
            assert!(request.contains("\r\nx-selected: selected.example:9443\r\n"));
            assert!(request.contains("\r\nx-forwarded-for: trusted, 127.0.0.1\r\n"));
            assert!(request.contains("\r\nx-forwarded-proto: http\r\n"));
            assert!(request.contains("\r\nx-request-id: request-1\r\n"));
            assert!(request.contains("\r\nx-mixed: first\r\nx-mixed: second\r\n"));
            assert!(request.contains("\r\ncookie: a=1\r\ncookie: b=2\r\n"));
            assert!(!request.contains("X-MiXeD:"));
            assert!(!request.contains("CoOkIe:"));
            assert!(!request.contains("x-hop:"));
            assert!(!request.contains("x-remove:"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncOnTeNt-LeNgTh: 2\r\nX-Remove: old\r\nsEt-CoOkIe: sid=1; Path=/internal; HttpOnly\r\nSET-cookie: theme=dark; Path=/\r\nX-MiXeD: retained\r\nX-Hop: secret\r\nConnection: close\r\n\r\nok",
                )
                .await
                .expect("policy origin response");
        });
        let mut policy = host_shaped_proxy_policy();
        policy.retry.max_retries = 0;
        let mut proxy_route = route(None, "/", &[], "origin");
        let HttpRouteAction::Proxy {
            policy: route_policy,
            ..
        } = &mut proxy_route.action
        else {
            unreachable!();
        };
        *route_policy = policy.into();
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![proxy_route],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request(
                "GET / HTTP/1.1\r\nhOsT: Client.Example.:8080\r\nX-Remove: client\r\nX-Forwarded-For: trusted\r\nX-Request-Id: request-1\r\nX-MiXeD: first\r\nx-mIXed: second\r\nCoOkIe: a=1\r\nCOOKIE: b=2\r\nX-Hop: secret\r\n",
            )
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.header("x-added"), Some("new"));
        assert_eq!(response.header("x-remove"), None);
        assert_eq!(
            response.header("set-cookie"),
            Some("theme=dark; Path=/")
        );
        let response_wire = response.text();
        assert!(response_wire.contains(
            "\r\nSet-Cookie: sid=1; Path=/; Secure; SameSite=Lax\r\nSet-Cookie: theme=dark; Path=/\r\n"
        ));
        assert!(response_wire.contains("\r\nx-mixed: retained\r\n"));
        assert!(response_wire.contains("\r\nContent-Length: 2\r\n"));
        assert!(!response_wire.contains("X-MiXeD:"));
        assert!(!response_wire.contains("X-Hop:"));

        proxy.finish().await;
        origin.await.expect("policy origin task");
    })
    .await
    .expect("proxy policy test timed out");
}

#[tokio::test]
async fn proxy_cookie_attribute_policy_replaces_and_removes_selected_flags() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("cookie policy origin bind");
        let origin_address = listener.local_addr().expect("cookie policy origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("cookie policy origin accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("cookie policy origin request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: session=one; Path=/; Secure; HttpOnly; SameSite=Strict\r\nSet-Cookie: other=two; Path=/\r\nConnection: close\r\n\r\nok",
                )
                .await
                .expect("cookie policy origin response");
        });
        let mut proxy_route = route(None, "/", &[], "origin");
        let HttpRouteAction::Proxy { policy, .. } = &mut proxy_route.action else {
            unreachable!();
        };
        policy.retry.max_retries = 0;
        policy.response_cookie_attributes = vec![HttpCookieAttributePolicy {
            name: "session".into(),
            secure: Some(false),
            http_only: Some(false),
            same_site: Some(HttpSameSite::Lax),
        }];
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![proxy_route],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: cookie-flags.test\r\n")
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        let wire = response.text();
        assert!(wire.contains("Set-Cookie: session=one; Path=/; SameSite=Lax"));
        assert!(wire.contains("Set-Cookie: other=two; Path=/"));
        assert!(!wire.contains("session=one; Path=/; Secure"));
        assert!(!wire.contains("session=one; Path=/; HttpOnly"));

        proxy.finish().await;
        origin.await.expect("cookie policy origin task");
    })
    .await
    .expect("cookie attribute policy test timed out");
}

#[tokio::test]
async fn x_forwarded_for_source_exception_preserves_the_incoming_header() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("XFF exception origin bind");
        let origin_address = listener.local_addr().expect("XFF exception origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("XFF origin accept");
            let request = String::from_utf8(
                read_request_head_bytes(&mut stream)
                    .await
                    .expect("XFF origin request"),
            )
            .expect("XFF request UTF-8");
            let request = request.to_ascii_lowercase();
            assert!(request.contains("\r\nx-forwarded-for: trusted\r\n"));
            assert!(!request.contains("trusted, 127.0.0.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("XFF origin response");
        });
        let mut policy = host_shaped_proxy_policy();
        let HttpRequestHeaderMutation::Set { value, .. } = &mut policy.request_headers[4] else {
            panic!("XFF mutation")
        };
        let HttpRequestHeaderValue::AppendedXForwardedFor {
            except_source_cidrs,
            ..
        } = value
        else {
            panic!("XFF value")
        };
        except_source_cidrs.push("127.0.0.0/8".into());
        policy.retry.max_retries = 0;
        let mut proxy_route = route(None, "/", &[], "origin");
        let HttpRouteAction::Proxy {
            policy: route_policy,
            ..
        } = &mut proxy_route.action
        else {
            panic!("proxy route")
        };
        *route_policy = policy.into();
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![proxy_route],
            1024,
            1,
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: client.example\r\nX-Forwarded-For: trusted\r\n")
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        proxy.finish().await;
        origin.await.expect("XFF origin task");
    })
    .await
    .expect("XFF exception test timed out");
}

fn host_shaped_proxy_policy() -> HttpProxyPolicy {
    HttpProxyPolicy {
        upstream_host: HttpUpstreamHost::Literal {
            value: "selected.example:9443".into(),
        },
        request_headers: vec![
            HttpRequestHeaderMutation::Set {
                name: "x-incoming".into(),
                value: HttpRequestHeaderValue::IncomingAuthority,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-normalized".into(),
                value: HttpRequestHeaderValue::NormalizedHost,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-selected".into(),
                value: HttpRequestHeaderValue::SelectedUpstreamHost,
            },
            HttpRequestHeaderMutation::Remove {
                name: "x-remove".into(),
            },
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-proto".into(),
                value: HttpRequestHeaderValue::DownstreamScheme,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-request-id".into(),
                value: HttpRequestHeaderValue::IncomingHeader {
                    name: "x-request-id".into(),
                    max_bytes: 32,
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-nginx-host".into(),
                value: HttpRequestHeaderValue::NginxHost {
                    fallback: "fallback.example".into(),
                },
            },
            HttpRequestHeaderMutation::Remove {
                name: "x-hop".into(),
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-added".into(),
                value: "new".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-remove".into(),
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-hop".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/internal".into(),
            to: "/".into(),
            mode: oxiroute_config::CookiePathMatch::Exact,
        }],
        response_cookie_attributes: vec![HttpCookieAttributePolicy {
            name: "sid".into(),
            secure: Some(true),
            http_only: Some(false),
            same_site: Some(HttpSameSite::Lax),
        }],
        ..HttpProxyPolicy::default()
    }
}

#[tokio::test]
async fn preserve_and_endpoint_host_policies_set_the_expected_upstream_authority() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Host-policy origin bind");
        let origin_address = listener.local_addr().expect("Host-policy origin address");
        let origin = tokio::spawn(async move {
            for expected in [
                "host: incoming.example:8080".to_owned(),
                format!("host: {origin_address}"),
            ] {
                let (mut stream, _) = listener.accept().await.expect("Host-policy origin accept");
                let request = read_request_head_bytes(&mut stream)
                    .await
                    .expect("Host-policy request");
                let request = String::from_utf8(request)
                    .expect("Host-policy request UTF-8")
                    .to_ascii_lowercase();
                assert!(request.contains(&expected), "request: {request}");
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .expect("Host-policy response");
            }
        });
        let preserve = route(None, "/preserve", &[], "origin");
        let mut endpoint = route(None, "/endpoint", &[], "origin");
        let HttpRouteAction::Proxy { policy, .. } = &mut endpoint.action else {
            unreachable!();
        };
        policy.upstream_host = HttpUpstreamHost::Endpoint {
            unix_fallback: None,
        };
        let proxy = ProxyHarness::start(
            vec![pool("origin", &[origin_address])],
            vec![preserve, endpoint],
            1024,
            2,
        )
        .await;

        for path in ["/preserve", "/endpoint"] {
            let response = proxy
                .request(&format!(
                    "GET {path} HTTP/1.1\r\nHost: Incoming.Example:8080\r\n"
                ))
                .await;
            assert_eq!(response.status, 200, "response: {}", response.text());
        }

        proxy.finish().await;
        origin.await.expect("Host-policy origin task");
    })
    .await
    .expect("Host-policy test timed out");
}

#[tokio::test]
async fn automatic_response_headers_follow_service_policy_on_h1_wire() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_without_connection_header("proxied", 1).await;
        let fixed = HttpRoute {
            host: None,
            path: HttpPathSelector::Exact {
                value: "/fixed".into(),
            },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "fixed".into(),
                headers: Vec::new(),
            },
        };
        let disabled = ProxyHarness::start_with_features(
            vec![pool("origin", &[origin.address])],
            vec![fixed.clone(), route(None, "/", &[], "origin")],
            Some(1024),
            false,
            100,
            0,
            false,
            2,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        for (path, body) in [("/fixed", b"fixed".as_slice()), ("/proxied", b"proxied")] {
            let response = disabled
                .request(&format!("GET {path} HTTP/1.1\r\nHost: headers.test\r\n"))
                .await;
            assert_eq!(response.status, 200);
            assert_eq!(response.body(), body);
            assert_eq!(response.header("date"), None);
            assert_eq!(
                response.header("connection"),
                None,
                "{path}: {}",
                response.text()
            );
        }
        disabled.finish().await;
        origin.finish().await;

        let enabled = ProxyHarness::start_with_features(
            Vec::new(),
            vec![fixed],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await;
        let response = enabled
            .request("GET /fixed HTTP/1.1\r\nHost: headers.test\r\n")
            .await;
        assert!(response.header("date").is_some());
        assert_eq!(response.header("connection"), Some("close"));
        enabled.finish().await;
    })
    .await
    .expect("automatic H1 response-header policy test timed out");
}

#[tokio::test]
async fn proxied_response_strips_upstream_hop_by_hop_headers_on_h1_wire() {
    timeout(TEST_TIMEOUT, async {
        let origin_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("origin bind");
        let origin_address = origin_listener.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = origin_listener.accept().await.expect("origin accept");
            read_request_head(&mut stream)
                .await
                .expect("origin request");
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 7\r\nConnection: close, X-Hop\r\nX-Hop: secret\r\nKeep-Alive: timeout=5\r\nProxy-Connection: keep-alive\r\nX-End-To-End: retained\r\n\r\nproxied",
                )
                .await
                .expect("origin response");
            stream.shutdown().await.expect("origin shutdown");
        });
        let proxy = ProxyHarness::start_with_features(
            vec![pool("origin", &[origin_address])],
            vec![route(None, "/", &[], "origin")],
            Some(1024),
            false,
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: headers.test\r\n")
            .await;
        assert_eq!(response.status, 202);
        assert_eq!(response.body(), b"proxied");
        assert_eq!(response.header("content-length"), Some("7"));
        assert_eq!(response.header("x-end-to-end"), Some("retained"));
        for name in ["connection", "x-hop", "keep-alive", "proxy-connection"] {
            assert_eq!(response.header(name), None, "{}", response.text());
        }

        proxy.finish().await;
        origin.await.expect("origin task");
    })
    .await
    .expect("hop-by-hop response-header test timed out");
}
