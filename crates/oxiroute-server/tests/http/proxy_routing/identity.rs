use crate::TEST_TIMEOUT;
use crate::ports_support;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::capturing_origin;
use tokio::time::timeout;

fn request_id_of(request: &str) -> Vec<&str> {
    request
        .lines()
        .filter(|line| line.to_ascii_lowercase().starts_with("x-request-id:"))
        .collect()
}

#[tokio::test]
async fn a_valid_incoming_request_id_reaches_the_origin_and_the_client() {
    timeout(TEST_TIMEOUT, async {
        let (origin, seen, task) = capturing_origin("").await;
        let proxy = ProxyHarness::start(
            vec![pool("rid", &[origin])],
            vec![route(None, "/", &[], "rid")],
            1024,
            1,
        )
        .await;
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: rid.test\r\nX-Request-Id: client-42\r\n")
            .await;
        assert_eq!(response.status, 200, "response: {}", response.text());
        assert_eq!(response.header("x-request-id"), Some("client-42"));
        let request = seen.await.expect("origin request");
        assert_eq!(
            request_id_of(&request),
            ["x-request-id: client-42"],
            "{request}"
        );
        proxy.finish().await;
        task.await.expect("origin task");
    })
    .await
    .expect("request id test timed out");
}

#[tokio::test]
async fn a_missing_or_malformed_request_id_is_replaced_by_a_generated_one() {
    timeout(TEST_TIMEOUT, async {
        for incoming in ["", "X-Request-Id: bad id;value\r\n"] {
            let (origin, seen, task) = capturing_origin("").await;
            let proxy = ProxyHarness::start(
                vec![pool("rid", &[origin])],
                vec![route(None, "/", &[], "rid")],
                1024,
                1,
            )
            .await;
            let response = proxy
                .request(&format!("GET / HTTP/1.1\r\nHost: rid.test\r\n{incoming}"))
                .await;
            let echoed = response
                .header("x-request-id")
                .expect("echoed id")
                .to_owned();
            assert!(echoed.starts_with("rq-"), "echoed: {echoed}");
            let request = seen.await.expect("origin request");
            assert_eq!(request_id_of(&request), [format!("x-request-id: {echoed}")]);
            proxy.finish().await;
            task.await.expect("origin task");
        }
    })
    .await
    .expect("generated request id test timed out");
}

#[tokio::test]
async fn an_id_the_origin_already_returns_is_kept_as_the_upstream_request_id() {
    timeout(TEST_TIMEOUT, async {
        let (origin, _seen, task) = capturing_origin("X-Request-Id: origin-side\r\n").await;
        let proxy = ProxyHarness::start(
            vec![pool("rid", &[origin])],
            vec![route(None, "/", &[], "rid")],
            1024,
            1,
        )
        .await;
        let response = proxy
            .request("GET / HTTP/1.1\r\nHost: rid.test\r\nX-Request-Id: client-1\r\n")
            .await;
        assert_eq!(response.header("x-request-id"), Some("client-1"));
        assert_eq!(
            response.header("x-upstream-request-id"),
            Some("origin-side")
        );
        proxy.finish().await;
        task.await.expect("origin task");
    })
    .await
    .expect("upstream request id test timed out");
}

#[tokio::test]
async fn locally_generated_responses_carry_the_request_id() {
    timeout(TEST_TIMEOUT, async {
        let dead = ports_support::unused_address();
        let proxy = ProxyHarness::start(
            vec![pool("dead", &[dead])],
            vec![route(Some("rid.test"), "/", &[], "dead")],
            1024,
            2,
        )
        .await;
        let unrouted = proxy
            .request("GET / HTTP/1.1\r\nHost: other.test\r\nX-Request-Id: nr-1\r\n")
            .await;
        assert_eq!(unrouted.status, 404);
        assert_eq!(unrouted.header("x-request-id"), Some("nr-1"));
        let failed = proxy
            .request("GET / HTTP/1.1\r\nHost: rid.test\r\nX-Request-Id: bad-gw\r\n")
            .await;
        assert_eq!(failed.status, 502, "response: {}", failed.text());
        assert_eq!(failed.header("x-request-id"), Some("bad-gw"));
        proxy.finish().await;
    })
    .await
    .expect("local response request id test timed out");
}
