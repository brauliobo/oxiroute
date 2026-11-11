//! Routes with retries and queue classes against the classic path: an origin that fails in scripted ways, the retry
//! policies that react to each failure, and what the client, the origin and the access log saw.

use std::time::Duration;

use oxiroute_config::{
    AccessLogPolicy, ConfigDraft, HttpProxyPolicy, HttpRetryBodySafety, HttpRetryBudget,
    HttpRetryMethodSafety, HttpRetryPolicy, HttpRetryTarget, HttpRetryTrigger, HttpRouteAction,
    PoolQueueClass, PoolQueuePolicy,
};

use super::{OK, Pair, features::log_lines, play};
use crate::h1_wire::{Origin, Step, Wire};

const R503: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\nX-Origin: down\r\n\r\ndown";
const R502: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 3\r\n\r\nbad";
const R404: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nno";
const JUNK: &[u8] = b"not http at all\r\n\r\n";

const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const HEAD: &[u8] = b"HEAD /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const POST: &[u8] = b"POST /r HTTP/1.1\r\nHost: example.test\r\nContent-Length: 0\r\n\r\n";
const POST_KEYED: &[u8] =
    b"POST /r HTTP/1.1\r\nHost: example.test\r\nContent-Length: 0\r\nIdempotency-Key: k1\r\n\r\n";
const POST_BODY: &[u8] = b"POST /r HTTP/1.1\r\nHost: example.test\r\nContent-Length: 3\r\n\r\nabc";

fn retry(
    max_retries: u8,
    response_statuses: &[u16],
    triggers: &[HttpRetryTrigger],
) -> HttpRetryPolicy {
    HttpRetryPolicy {
        max_retries,
        response_statuses: response_statuses.to_vec(),
        triggers: triggers.to_vec(),
        target: HttpRetryTarget::SameServer,
        ..HttpRetryPolicy::default()
    }
}

fn with_retry(retry: HttpRetryPolicy) -> impl Fn(&mut ConfigDraft) {
    move |config| route_policy(config, |policy| policy.retry = retry.clone())
}

fn route_policy(config: &mut ConfigDraft, edit: impl FnOnce(&mut HttpProxyPolicy)) {
    let route = config.http_services[0]
        .routes
        .last_mut()
        .expect("the proxy route");
    let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
        panic!("the last route proxies");
    };
    edit(policy);
}

/// Both proxies in front of an origin that follows `steps` (the first answers the warm-up request), with an access
/// log each; the origin's requests, the client's answers and the access lines are the same on both.
async fn scenario(
    steps: &'static [Step],
    edit: impl Fn(&mut ConfigDraft),
    script: &[&[u8]],
) -> Outcomes {
    let directory = tempfile::tempdir().expect("log directory");
    let [classic_log, lane_log] =
        ["classic.log", "lane.log"].map(|name| directory.path().join(name));
    let (classic_path, lane_path) = (classic_log.clone(), lane_log.clone());
    let classic = Wire::over(Origin::scripted(steps).await, |config| {
        super::with_fixed_route(config);
        edit(config);
        config.http_services[0].access_log = Some(AccessLogPolicy::File { path: classic_path });
        config.http_fast_lane = Some(false);
    })
    .await;
    let lane = Wire::over(Origin::scripted(steps).await, |config| {
        super::with_fixed_route(config);
        edit(config);
        config.http_services[0].access_log = Some(AccessLogPolicy::File { path: lane_path });
    })
    .await;
    let pair = Pair { classic, lane }.warmed().await;
    let classic = play(&pair.classic, script).await;
    let lane = play(&pair.lane, script).await;
    let (classic_lines, lane_lines) = (log_lines(&classic_log).await, log_lines(&lane_log).await);
    pair.finish();
    Outcomes {
        classic,
        lane,
        classic_lines,
        lane_lines,
    }
}

struct Outcomes {
    classic: super::Outcome,
    lane: super::Outcome,
    classic_lines: Vec<String>,
    lane_lines: Vec<String>,
}

impl Outcomes {
    /// Both paths did the same; returns what the client got and how many requests the origin saw.
    fn identical(&self, name: &str) -> (&str, usize) {
        assert_eq!(self.classic, self.lane, "{name}");
        assert_eq!(self.classic_lines, self.lane_lines, "{name}: access lines");
        (&self.classic.response, self.classic.origin.len())
    }
}

#[tokio::test]
async fn a_status_the_route_retries_on_is_retried_until_the_origin_answers() {
    const STEPS: &[Step] = &[
        Step::Reply(OK),
        Step::Reply(R503),
        Step::Reply(R503),
        Step::Reply(OK),
    ];
    let outcomes = scenario(STEPS, with_retry(retry(2, &[503], &[])), &[GET]).await;
    let (response, attempts) = outcomes.identical("two 503 then 200");
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert_eq!(attempts, 3);
}

#[tokio::test]
async fn retries_that_run_out_relay_the_last_answer() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Reply(R503)];
    let outcomes = scenario(STEPS, with_retry(retry(2, &[503], &[])), &[GET, GET]).await;
    let (response, attempts) = outcomes.identical("503 forever");
    assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
    assert_eq!(attempts, 6);
}

#[tokio::test]
async fn a_status_the_route_does_not_retry_on_is_relayed_at_once() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Reply(R404), Step::Reply(OK)];
    let outcomes = scenario(STEPS, with_retry(retry(2, &[503], &[])), &[GET]).await;
    let (response, attempts) = outcomes.identical("404");
    assert!(response.starts_with("HTTP/1.1 404 "), "{response}");
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn an_origin_that_closes_without_answering_is_retried_on_the_empty_response_trigger() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Drop, Step::Reply(OK)];
    let outcomes = scenario(
        STEPS,
        with_retry(retry(1, &[], &[HttpRetryTrigger::EmptyResponse])),
        &[GET],
    )
    .await;
    let (response, attempts) = outcomes.identical("empty response");
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert_eq!(attempts, 2);
    let outcomes = scenario(
        STEPS,
        with_retry(retry(1, &[], &[HttpRetryTrigger::ConnectFailure])),
        &[GET],
    )
    .await;
    let (response, attempts) = outcomes.identical("empty response without the trigger");
    assert!(response.starts_with("HTTP/1.1 502 "), "{response}");
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn a_junk_answer_is_retried_on_the_junk_response_trigger() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Reply(JUNK), Step::Reply(OK)];
    let outcomes = scenario(
        STEPS,
        with_retry(retry(1, &[], &[HttpRetryTrigger::JunkResponse])),
        &[GET],
    )
    .await;
    let (response, attempts) = outcomes.identical("junk response");
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn a_slow_answer_is_retried_on_the_response_timeout_trigger() {
    const STEPS: &[Step] = &[
        Step::Reply(OK),
        Step::Slow(Duration::from_millis(1000), OK),
        Step::Reply(OK),
    ];
    let timeouts = |config: &mut ConfigDraft| {
        route_policy(config, |policy| {
            policy.retry = retry(1, &[], &[HttpRetryTrigger::ResponseTimeout]);
            policy.timeouts.header_ms = Some(100);
        });
    };
    let outcomes = scenario(STEPS, timeouts, &[GET]).await;
    let (response, attempts) = outcomes.identical("response timeout");
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn only_requests_that_can_be_sent_again_are_retried() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Reply(R502), Step::Reply(OK)];
    for (name, request, retried) in [
        ("GET", GET, true),
        ("HEAD", HEAD, true),
        ("POST", POST, false),
        ("POST with a body", POST_BODY, false),
    ] {
        let outcomes = scenario(STEPS, with_retry(retry(1, &[502], &[])), &[request]).await;
        let (_, attempts) = outcomes.identical(name);
        assert_eq!(attempts, if retried { 2 } else { 1 }, "{name}");
    }
}

/// Retries of every method replay buffered bodies: a route that asks for them is served by the classic path on both.
#[tokio::test]
async fn retries_of_every_method_are_the_same_on_both_paths() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Reply(R502), Step::Reply(OK)];
    let policy = |config: &mut ConfigDraft| {
        let mut policy = retry(1, &[502], &[]);
        policy.method_safety = HttpRetryMethodSafety::All;
        policy.body_safety = HttpRetryBodySafety::Buffered;
        policy.idempotency_header = Some("idempotency-key".into());
        route_policy(config, |route| route.retry = policy.clone());
        let route = config.http_services[0]
            .routes
            .last_mut()
            .expect("the proxy route");
        route.policy.request_buffering = true;
    };
    for (name, request, attempts) in [
        ("keyed", POST_KEYED, 2),
        ("unkeyed", POST, 1),
        ("body", POST_BODY, 1),
    ] {
        let outcomes = scenario(STEPS, policy, &[request]).await;
        assert_eq!(outcomes.identical(name).1, attempts, "{name}");
    }
}

/// The first request spends the only retry of the second; the next one is told what the origin said.
#[tokio::test]
async fn a_spent_budget_stops_the_retries() {
    const STEPS: &[Step] = &[
        Step::Reply(OK),
        Step::Reply(R503),
        Step::Reply(OK),
        Step::Reply(R503),
    ];
    let policy = |config: &mut ConfigDraft| {
        let mut policy = retry(2, &[503], &[]);
        policy.budget = Some(HttpRetryBudget {
            per_second: Some(1),
            ratio_percent: None,
        });
        with_retry(policy)(config);
    };
    let outcomes = scenario(STEPS, policy, &[GET, GET]).await;
    let (response, attempts) = outcomes.identical("a budget of one retry");
    assert_eq!(response.matches("HTTP/1.1 200 ").count(), 1, "{response}");
    assert_eq!(response.matches("HTTP/1.1 503 ").count(), 1, "{response}");
    assert_eq!(attempts, 3);
}

#[tokio::test]
async fn a_retry_waits_the_delay_of_the_route() {
    const STEPS: &[Step] = &[Step::Reply(OK), Step::Reply(R503), Step::Reply(OK)];
    let policy = |config: &mut ConfigDraft| {
        let mut policy = retry(1, &[503], &[]);
        policy.delay_ms = 100;
        with_retry(policy)(config);
    };
    let outcomes = scenario(STEPS, policy, &[GET]).await;
    let (_, attempts) = outcomes.identical("a delay");
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn a_connection_that_fails_is_retried_on_another_server() {
    const STEPS: &[Step] = &[Step::Reply(OK)];
    let dead = crate::process_support::reserve_tcp_address();
    let two_servers = move |config: &mut ConfigDraft| {
        let mut dead_server = config.upstream_pools[0].servers[0].clone();
        dead_server.name = "dead".into();
        dead_server.endpoint = crate::config_support::socket_endpoint(dead);
        config.upstream_pools[0].servers.insert(0, dead_server);
        let mut policy = retry(1, &[], &[HttpRetryTrigger::ConnectFailure]);
        policy.target = HttpRetryTarget::NextServer;
        with_retry(policy)(config);
    };
    let outcomes = scenario(STEPS, two_servers, &[GET, GET, GET]).await;
    let (response, attempts) = outcomes.identical("a dead server first");
    assert!(response.matches("HTTP/1.1 200 ").count() == 3, "{response}");
    assert_eq!(attempts, 3);
}

/// A route with retries that nothing triggers costs nothing: the requests are as they are without them.
#[tokio::test]
async fn routes_with_retries_pass_the_plain_corpus() {
    let pair = Pair::configured(OK, false, false, with_retry(retry(2, &[502, 503], &[]))).await;
    super::compare(&pair, &super::corpus::requests("/r")).await;
    pair.finish();
}

fn with_queue(config: &mut ConfigDraft) {
    let pool = &mut config.upstream_pools[0];
    pool.queue_timeout_ms = Some(2_000);
    pool.servers[0].max_connections = Some(1);
    pool.queue = Some(PoolQueuePolicy {
        classes: ["high", "low"]
            .map(|name| PoolQueueClass {
                name: name.into(),
                weight: 1,
            })
            .to_vec(),
        class_header: Some("x-class".into()),
        tenant_header: Some("x-tenant".into()),
        ..PoolQueuePolicy::default()
    });
    route_policy(config, |policy| {
        policy.queue_headers = true;
    });
}

/// A request that waits for the one connection of the server is told its place and its wait; the class and tenant
/// headers place it as on the classic path.
#[tokio::test]
async fn requests_that_queue_are_told_their_place_on_both_paths() {
    const STEPS: &[Step] = &[
        Step::Reply(OK),
        Step::Slow(Duration::from_millis(600), OK),
        Step::Reply(OK),
    ];
    let directory = tempfile::tempdir().expect("log directory");
    let mut responses = Vec::new();
    for fast_lane in [false, true] {
        let log = directory.path().join(format!("{fast_lane}.log"));
        let wire = Wire::over(Origin::scripted(STEPS).await, |config| {
            with_queue(config);
            config.http_services[0].access_log = Some(AccessLogPolicy::File { path: log });
            if !fast_lane {
                config.http_fast_lane = Some(false);
            }
        })
        .await;
        wire.exchange(b"GET /r HTTP/1.1\r\nHost: warm.test\r\n\r\n")
            .await;
        let first = tokio::spawn({
            let address = wire.proxy;
            async move {
                let wire_request = b"GET /r HTTP/1.1\r\nHost: example.test\r\nX-Class: low\r\n\r\n";
                raw_exchange(address, wire_request).await
            }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let second = raw_exchange(
            wire.proxy,
            b"GET /r HTTP/1.1\r\nHost: example.test\r\nX-Class: high\r\nX-Tenant: t1\r\n\r\n",
        )
        .await;
        let first = first.await.expect("first request");
        responses.push((mask_wait(&first), mask_wait(&second)));
        wire.finish();
    }
    assert_eq!(responses[0], responses[1]);
    assert!(
        responses[0].1.contains("x-oxiroute-queue-position: 1"),
        "{}",
        responses[0].1
    );
    assert!(
        !responses[0].0.contains("x-oxiroute-queue"),
        "{}",
        responses[0].0
    );
}

async fn raw_exchange(address: std::net::SocketAddr, request: &[u8]) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    stream.write_all(request).await.expect("write");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match tokio::time::timeout(Duration::from_millis(1500), stream.read(&mut buffer)).await {
            Ok(Ok(count)) if count > 0 => bytes.extend_from_slice(&buffer[..count]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The wait is a measured time; everything else is compared as it is. Dates and request ids differ by construction.
fn mask_wait(response: &str) -> String {
    response
        .lines()
        .filter(|line| !line.to_ascii_lowercase().starts_with("date:"))
        .filter(|line| !line.to_ascii_lowercase().starts_with("x-request-id:"))
        .map(|line| {
            if line
                .to_ascii_lowercase()
                .starts_with("x-oxiroute-queue-wait-ms:")
            {
                "x-oxiroute-queue-wait-ms: <ms>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
