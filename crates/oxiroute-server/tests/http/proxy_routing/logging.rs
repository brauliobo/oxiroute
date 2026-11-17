use crate::TEST_TIMEOUT;
use crate::fixture_support::write_secure_token;
use crate::http_support::raw_http_request;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::ports_support;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::capturing_origin;
use oxiroute_config::AccessLogPolicy;
use oxiroute_config::DownstreamTimeoutPolicy;
use oxiroute_config::HttpAccessPolicy;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio::time::timeout;

#[tokio::test]
async fn structured_access_log_omits_authorization_cookies_and_query_tokens() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("access log directory");
        let path = directory.path().join("access.jsonl");
        let token = "0123456789abcdef0123456789abcdef";
        let token_path = write_secure_token(directory.path(), "access-log-token", token);
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::Exact {
                value: "/private".into(),
            },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path: token_path,
                header_name: "authorization".into(),
                realm: None,
            }),
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 204,
                body: String::new(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start_with_features(
            Vec::new(),
            vec![route],
            Some(1024),
            true,
            100,
            0,
            false,
            4,
            None,
            Some(AccessLogPolicy::File { path: path.clone() }),
            DownstreamTimeoutPolicy::default(),
        )
        .await;

        let malformed = proxy
            .request("GET /private HTTP/1.1\r\nHost: first.test\r\nHost: second.test\r\n")
            .await;
        assert_eq!(malformed.status, 400);
        let unmatched = proxy
            .request("GET /missing HTTP/1.1\r\nHost: log.test\r\n")
            .await;
        assert_eq!(unmatched.status, 404);
        let unauthorized = proxy
            .request("GET /private HTTP/1.1\r\nHost: log.test\r\n")
            .await;
        assert_eq!(unauthorized.status, 401);
        let response = proxy
            .request(&format!(
                "GET /private?token=query-secret HTTP/1.1\r\nHost: log.test\r\nAuthorization: Bearer {token}\r\nCookie: session=cookie-secret\r\n"
            ))
            .await;
        assert_eq!(response.status, 204);
        proxy.finish().await;

        let contents = fs::read_to_string(path).expect("read access log");
        let events = contents
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("access event"))
            .collect::<Vec<_>>();
        // The duplicate Host head is refused while it is parsed, before a request exists to log.
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["route"], serde_json::Value::Null);
        assert_eq!(events[0]["status"], 404);
        assert_eq!(events[1]["route"], "0");
        assert_eq!(events[1]["status"], 401);
        assert_eq!(events[2]["service"], "routing");
        assert_eq!(events[2]["route"], "0");
        assert_eq!(events[2]["host"], "log.test");
        assert_eq!(events[2]["method"], "GET");
        assert_eq!(events[2]["status"], 204);
        for secret in ["query-secret", token, "cookie-secret", "Authorization", "Cookie"] {
            assert!(!contents.contains(secret), "access log exposed {secret}");
        }
    })
    .await
    .expect("access log test timed out");
}

async fn access_log_events(path: &Path) -> Vec<serde_json::Value> {
    for _ in 0..50 {
        let contents = fs::read_to_string(path).unwrap_or_default();
        if !contents.is_empty() {
            return contents
                .lines()
                .map(|line| serde_json::from_str(line).expect("access event"))
                .collect();
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("access log stayed empty");
}

#[tokio::test]
async fn access_log_names_request_pool_server_attempts_and_hashes_the_idempotency_key() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("access log directory");
        let path = directory.path().join("access.jsonl");
        let (origin, _seen, task) = capturing_origin("").await;
        let proxy = ProxyHarness::start_with_features(
            vec![pool("traced", &[origin])],
            vec![route(None, "/", &[], "traced")],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            None,
            Some(AccessLogPolicy::File { path: path.clone() }),
            DownstreamTimeoutPolicy::default(),
        )
        .await;
        let response = proxy
            .request(
                "GET / HTTP/1.1\r\nHost: log.test\r\nX-Request-Id: trace-me\r\nIdempotency-Key: order-8841-secret\r\n",
            )
            .await;
        assert_eq!(response.status, 200);
        proxy.finish().await;
        task.await.expect("origin task");

        let events = access_log_events(&path).await;
        let event = &events[0];
        assert_eq!(event["requestId"], "trace-me");
        assert_eq!(event["correlationId"], "trace-me");
        assert_eq!(event["pool"], "traced");
        assert_eq!(event["server"], "endpoint-1");
        assert_eq!(event["servers"], "endpoint-1");
        assert_eq!(event["attempts"], 1);
        assert_eq!(event["reason"], "");
        assert_eq!(event["rescued"], false);
        assert!(event["queueWaitMs"].is_string() && event["headerMs"].is_string());
        let tag = event["idemKey"].as_str().expect("idempotency tag");
        assert_eq!(tag.len(), 8);
        assert!(!fs::read_to_string(&path).expect("log").contains("order-8841"));
    })
    .await
    .expect("traced access log test timed out");
}

#[tokio::test]
async fn access_log_gives_the_reason_a_request_failed() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("access log directory");
        let path = directory.path().join("access.jsonl");
        let dead = ports_support::unused_address();
        let proxy = ProxyHarness::start_with_features(
            vec![pool("dead", &[dead])],
            vec![route(None, "/", &[], "dead")],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            None,
            Some(AccessLogPolicy::File { path: path.clone() }),
            DownstreamTimeoutPolicy::default(),
        )
        .await;
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: log.test\r\nX-Request-Id: doomed\r\n")
            .await;
        assert_eq!(response.status, 502, "response: {}", response.text());
        proxy.finish().await;

        let events = access_log_events(&path).await;
        let event = &events[0];
        assert_eq!(event["requestId"], "doomed");
        assert_eq!(event["status"], 502);
        assert_eq!(event["reason"], "connect_failed");
        assert_eq!(event["pool"], "dead");
        assert_eq!(event["attempts"], 1);
    })
    .await
    .expect("failed access log test timed out");
}

fn live_request_phase(id: &str) -> Option<String> {
    oxiroute_server::live_requests()
        .into_iter()
        .find(|(live, _)| live == id)
        .map(|(_, phase)| phase)
}

async fn wait_for_live_request(id: &str, present: bool) {
    while live_request_phase(id).is_some() != present {
        sleep(Duration::from_millis(10)).await;
    }
}

/// Waits for the phase a request settles in while something outside the proxy holds it there.
async fn wait_for_live_phase(id: &str, phase: &str) {
    while live_request_phase(id).as_deref() != Some(phase) {
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_request_is_visible_while_it_waits_for_the_origin_and_gone_when_it_ends() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let origin = listener.local_addr().expect("origin address");
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (received_tx, received_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            read_request_head_bytes(&mut stream).await.expect("origin request");
            received_tx.send(()).expect("origin received the request");
            release_rx.await.expect("release");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("origin response");
        });
        let proxy = ProxyHarness::start(
            vec![pool("live", &[origin])],
            vec![route(None, "/", &[], "live")],
            1024,
            1,
        )
        .await;
        let address = proxy.address;
        let client = tokio::spawn(async move {
            raw_http_request(
                address,
                b"GET / HTTP/1.1\r\nHost: live.test\r\nX-Request-Id: live-1\r\nConnection: close\r\n\r\n",
            )
            .await
        });
        received_rx.await.expect("origin received the request");
        wait_for_live_phase("live-1", "awaiting_headers").await;
        release_tx.send(()).expect("release origin");
        assert_eq!(client.await.expect("client").status, 200);
        wait_for_live_request("live-1", false).await;
        proxy.finish().await;
        task.await.expect("origin task");
    })
    .await
    .expect("live request test timed out");
}

#[tokio::test]
async fn a_request_that_fails_leaves_the_registry_too() {
    timeout(TEST_TIMEOUT, async {
        let dead = ports_support::unused_address();
        let proxy = ProxyHarness::start(
            vec![pool("dead", &[dead])],
            vec![route(None, "/", &[], "dead")],
            1024,
            1,
        )
        .await;
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: live.test\r\nX-Request-Id: live-fail\r\n")
            .await;
        assert_eq!(response.status, 502);
        wait_for_live_request("live-fail", false).await;
        proxy.finish().await;
    })
    .await
    .expect("failed live request test timed out");
}
