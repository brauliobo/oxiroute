use crate::TEST_TIMEOUT;
use crate::fixture_support::create_secure_root;
use crate::support::harness::ProxyHarness;
use oxiroute_config::HttpLiteralHeader;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpRedirectLocation;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::HttpStaticErrorResponse;
use oxiroute_config::HttpStaticMimePolicy;
use oxiroute_config::HttpStaticPathMapping;
use std::fs;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one wire scenario covers fixed and redirect status/header semantics"
)]
async fn serves_fixed_and_redirect_actions_with_exact_get_and_head_semantics() {
    timeout(TEST_TIMEOUT, async {
        let proxy = ProxyHarness::start(
            Vec::new(),
            vec![
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/fixed".into(),
                    },
                    methods: vec!["GET".into(), "HEAD".into()],
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 200,
                        body: "fixed body".into(),
                        headers: vec![HttpLiteralHeader {
                            name: "x-fixed".into(),
                            value: "yes".into(),
                            always: true,
                        }],
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/empty".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 204,
                        body: String::new(),
                        headers: Vec::new(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/fixed-error".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 404,
                        body: "missing".into(),
                        headers: vec![
                            HttpLiteralHeader {
                                name: "x-selected-status".into(),
                                value: "no".into(),
                                always: false,
                            },
                            HttpLiteralHeader {
                                name: "x-always".into(),
                                value: "yes".into(),
                                always: true,
                            },
                        ],
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/redirect".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: oxiroute_config::HttpRoutePolicy::default(),
                    action: HttpRouteAction::Redirect {
                        status: 308,
                        location: HttpRedirectLocation::RequestTemplate {
                            value: "$scheme://$host$request_uri".into(),
                            nginx_host_fallback: None,
                        },
                        headers: vec![HttpLiteralHeader {
                            name: "x-redirect".into(),
                            value: "yes".into(),
                            always: false,
                        }],
                    },
                },
            ],
            1024,
            5,
        )
        .await;

        let get = proxy
            .request("GET /fixed HTTP/1.1\r\nHost: Actions.Example:8080\r\n")
            .await;
        assert_eq!(get.status, 200);
        assert_eq!(get.header("x-fixed"), Some("yes"));
        assert_eq!(get.header("content-length"), Some("10"));
        assert_eq!(get.body(), b"fixed body");

        let head = proxy
            .request("HEAD /fixed HTTP/1.1\r\nHost: actions.example:8080\r\n")
            .await;
        assert_eq!(head.status, 200);
        assert_eq!(head.header("content-length"), Some("10"));
        assert!(head.body().is_empty());

        let empty = proxy
            .request("GET /empty HTTP/1.1\r\nHost: actions.example\r\n")
            .await;
        assert_eq!(empty.status, 204);
        assert_eq!(empty.header("content-length"), None);
        assert!(empty.body().is_empty());

        let fixed_error = proxy
            .request("GET /fixed-error HTTP/1.1\r\nHost: actions.example\r\n")
            .await;
        assert_eq!(fixed_error.status, 404);
        assert!(fixed_error.header("x-selected-status").is_none());
        assert_eq!(fixed_error.header("x-always"), Some("yes"));

        let redirect = proxy
            .request("GET /redirect?next=1 HTTP/1.1\r\nHost: Actions.Example.:8080\r\n")
            .await;
        assert_eq!(redirect.status, 308);
        assert_eq!(
            redirect.header("location"),
            Some("http://actions.example/redirect?next=1")
        );
        assert_eq!(redirect.header("x-redirect"), Some("yes"));

        proxy.finish().await;
    })
    .await
    .expect("local action test timed out");
}

#[tokio::test]
async fn fixed_responses_have_one_wire_shape_and_keep_the_connection_alive() {
    timeout(TEST_TIMEOUT, async {
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::Exact {
                value: "/fixed".into(),
            },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "fixed body".into(),
                headers: vec![
                    HttpLiteralHeader {
                        name: "content-type".into(),
                        value: "text/plain".into(),
                        always: true,
                    },
                    HttpLiteralHeader {
                        name: "x-fixed".into(),
                        value: "yes".into(),
                        always: true,
                    },
                ],
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 1).await;
        let mut stream = TcpStream::connect(proxy.address)
            .await
            .expect("connect to the proxy");
        for (connection, tail) in [("keep-alive", ""), ("close", "Connection: close\r\n")] {
            let request = format!(
                "GET /fixed HTTP/1.1\r\nHost: wire.test\r\nX-Request-Id: wire-7\r\n{tail}\r\n"
            );
            stream.write_all(request.as_bytes()).await.expect("send");
            let mut received = Vec::new();
            while !received.ends_with(b"fixed body") {
                let mut chunk = [0_u8; 512];
                let count = stream.read(&mut chunk).await.expect("read the response");
                assert_ne!(count, 0, "closed before the body: {received:?}");
                received.extend_from_slice(&chunk[..count]);
            }
            let text = String::from_utf8(received).expect("ASCII response");
            let (head, body) = text.split_once("\r\n\r\n").expect("head and body");
            let lines: Vec<_> = head.split("\r\n").collect();
            assert_eq!(
                lines[..4],
                [
                    "HTTP/1.1 200 OK",
                    "Content-Type: text/plain",
                    "x-fixed: yes",
                    "Content-Length: 10",
                ]
            );
            assert_eq!(lines[4], "x-request-id: wire-7");
            assert!(
                lines[5].starts_with("Date: ") && lines[5].ends_with(" GMT"),
                "{}",
                lines[5]
            );
            assert_eq!(lines[6], format!("Connection: {connection}"));
            assert_eq!(lines.len(), 7);
            assert_eq!(body, "fixed body");
        }
        let mut rest = Vec::new();
        stream
            .read_to_end(&mut rest)
            .await
            .expect("closed after the second response");
        assert!(rest.is_empty());
        proxy.finish().await;
    })
    .await
    .expect("fixed wire test timed out");
}

#[tokio::test]
async fn serves_static_headers_and_internal_error_page_on_h1_wire() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("static response directory");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("ok.txt"), b"ok").expect("write static success file");
        fs::write(root.join("404.html"), b"missing").expect("write static error file");
        fs::write(root.join("405.html"), b"method not allowed").expect("write method error file");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::StaticFiles {
                root_directory: root,
                path_mapping: HttpStaticPathMapping::Root,
                index_files: Vec::new(),
                internal_index_redirects: true,
                directory_redirects: true,
                spa_fallback: None,
                try_files: Vec::new(),
                autoindex: false,
                autoindex_exact_size: true,
                autoindex_local_time: false,
                etag: true,
                mime: HttpStaticMimePolicy {
                    default_type: Some("text/plain".into()),
                    types: Vec::new(),
                },
                headers: vec![
                    HttpLiteralHeader {
                        name: "x-selected-status".into(),
                        value: "no".into(),
                        always: false,
                    },
                    HttpLiteralHeader {
                        name: "x-always".into(),
                        value: "yes".into(),
                        always: true,
                    },
                ],
                error_responses: vec![
                    HttpStaticErrorResponse {
                        statuses: vec![404],
                        file: Some("404.html".into()),
                        body: None,
                        headers: vec![HttpLiteralHeader {
                            name: "x-error".into(),
                            value: "yes".into(),
                            always: true,
                        }],
                        internal_redirect: Some("/404.html".into()),
                    },
                    HttpStaticErrorResponse {
                        statuses: vec![405],
                        file: Some("405.html".into()),
                        body: None,
                        headers: vec![HttpLiteralHeader {
                            name: "x-method-error".into(),
                            value: "yes".into(),
                            always: true,
                        }],
                        internal_redirect: Some("/405.html".into()),
                    },
                ],
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 4).await;

        let success = proxy
            .request("GET /ok.txt HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(success.status, 200);
        assert_eq!(success.header("x-selected-status"), Some("no"));
        assert_eq!(success.header("x-always"), Some("yes"));
        assert_eq!(success.header("x-error"), None);
        assert_eq!(success.body(), b"ok");

        let missing = proxy
            .request("GET /missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(missing.status, 404);
        assert_eq!(missing.header("x-selected-status"), None);
        assert_eq!(missing.header("x-always"), Some("yes"));
        assert_eq!(missing.header("x-error"), Some("yes"));
        assert_eq!(missing.body(), b"missing");

        let head = proxy
            .request("HEAD /missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(head.status, 404);
        assert_eq!(head.header("content-length"), Some("7"));
        assert!(head.body().is_empty());

        let method = proxy
            .request("POST /missing HTTP/1.1\r\nHost: static.test\r\nContent-Length: 0\r\n")
            .await;
        assert_eq!(method.status, 405);
        assert_eq!(method.header("x-method-error"), Some("yes"));
        assert_eq!(method.body(), b"method not allowed");

        proxy.finish().await;
    })
    .await
    .expect("static response H1 test timed out");
}
