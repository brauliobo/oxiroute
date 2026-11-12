//! HTTP/3 to origins that fail: the statuses, the retries and the health signals of the classic path.
#[allow(clippy::wildcard_imports)]
use super::*;

fn retrying(max_retries: u8) -> HttpProxyPolicy {
    let mut policy = HttpProxyPolicy::default();
    policy.retry.max_retries = max_retries;
    policy
}

fn dead_address() -> SocketAddr {
    process_support::ports::unused_address()
}

#[tokio::test]
pub(super) async fn h3_retries_a_connect_failure_on_the_next_server() {
    let (origin, healthy) = ScriptedOrigin::tcp(|_| ok(b"healthy")).await;
    let mut daemon = daemon_for(&[dead_address(), healthy], retrying(1)).await;
    for _ in 0..4 {
        let answer = daemon.get("/retry").await;
        assert_eq!((answer.status, answer.text().as_str()), (200, "healthy"));
    }
    assert_eq!(origin.requests().len(), 4);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_retries_a_post_that_never_reached_the_server() {
    let (origin, healthy) = ScriptedOrigin::tcp(|request| ok(&request.body)).await;
    let mut daemon = daemon_for(&[dead_address(), healthy], retrying(1)).await;
    for _ in 0..2 {
        let answer = daemon.send("POST", "/retry", &[], b"payload").await;
        assert_eq!((answer.status, answer.text().as_str()), (200, "payload"));
    }
    assert_eq!(origin.requests().len(), 2);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_answers_bad_gateway_when_no_server_can_be_reached() {
    let mut daemon = daemon_for(&[dead_address()], HttpProxyPolicy::default()).await;
    assert_eq!(daemon.get("/dead").await.status, 502);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_passive_health_ejects_a_server_that_fails_to_connect() {
    let (_origin, healthy) = ScriptedOrigin::tcp(|_| ok(b"healthy")).await;
    let listener = reserve_udp_address();
    let mut pool = tcp_pool("origin", &[dead_address(), healthy]);
    pool.passive_health = Some(oxiroute_config::PassiveHealthPolicy {
        error_limit: 1,
        ..Default::default()
    });
    let config = config(
        listener,
        service(vec![route("/", "origin", retrying(0))]),
        vec![pool],
    );
    let mut daemon = Daemon::start(listener, &config).await;
    let mut statuses = Vec::new();
    for _ in 0..6 {
        statuses.push(daemon.get("/passive").await.status);
    }
    assert_eq!(
        statuses.iter().filter(|status| **status == 502).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|status| **status == 200).count(),
        5,
        "{statuses:?}"
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_answers_gateway_timeout_when_the_origin_is_too_slow() {
    let (origin, address) = ScriptedOrigin::tcp(|_| {
        Reply::After(Duration::from_secs(3), response("200 OK", &[], b"late"))
    })
    .await;
    let mut policy = HttpProxyPolicy::default();
    policy.timeouts.header_ms = Some(300);
    let mut daemon = daemon_for(&[address], policy).await;
    let started = std::time::Instant::now();
    assert_eq!(daemon.get("/slow").await.status, 504);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        origin.requests().len(),
        1,
        "a response timeout is not retried by default"
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_answers_bad_gateway_when_the_origin_breaks_its_answer() {
    let (_origin, address) = ScriptedOrigin::tcp(|request| match request.target.as_str() {
        "/mid-body" => {
            Reply::SendAndClose(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nhalf".to_vec())
        }
        "/silent" => Reply::SendAndClose(Vec::new()),
        "/junk" => Reply::SendAndClose(b"not http at all\r\n\r\n".to_vec()),
        "/upgrade" => Reply::Send(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: x\r\nConnection: Upgrade\r\n\r\n"
                .to_vec(),
        ),
        _ => ok(b"fine"),
    })
    .await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    for path in ["/mid-body", "/silent", "/junk", "/upgrade"] {
        assert_eq!(daemon.get(path).await.status, 502, "{path}");
    }
    assert_eq!(daemon.get("/fine").await.status, 200);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_resets_an_answer_declared_beyond_the_buffer_bound_when_its_origin_ends_early()
 {
    let (_origin, address) = ScriptedOrigin::tcp(|_| {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            65 * 1024 * 1024
        );
        Reply::SendAndClose(head.into_bytes())
    })
    .await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    let mut stream = daemon.open("GET", "/huge", &[], b"").await;
    let ended = match stream.recv_response().await {
        Ok(response) => {
            assert_eq!(response.status(), 200);
            loop {
                match stream.recv_data().await {
                    Ok(Some(_)) => {}
                    other => break other.map(|_| ()),
                }
            }
        }
        Err(error) => Err(error),
    };
    assert!(
        ended.is_err(),
        "the response is reset, not finished: {ended:?}"
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_retries_a_retryable_status_of_an_h1_origin_on_another_server() {
    let (failing, first) =
        ScriptedOrigin::tcp(|_| Reply::Send(response("503 Service Unavailable", &[], b"down")))
            .await;
    let (healthy, second) = ScriptedOrigin::tcp(|_| ok(b"up")).await;
    let mut policy = retrying(1);
    policy.retry.response_statuses = vec![503];
    let mut daemon = daemon_for(&[first, second], policy).await;
    for _ in 0..2 {
        let answer = daemon.get("/status").await;
        assert_eq!((answer.status, answer.text().as_str()), (200, "up"));
    }
    assert!(!failing.requests().is_empty());
    assert_eq!(healthy.requests().len(), 2);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_goes_on_serving_after_a_client_gives_up_on_a_slow_origin() {
    let (origin, address) = ScriptedOrigin::tcp(|request| {
        if request.target == "/slow" {
            Reply::After(Duration::from_millis(400), response("200 OK", &[], b"late"))
        } else {
            ok(b"quick")
        }
    })
    .await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    let mut stream = daemon.open("GET", "/slow", &[], b"").await;
    stream.stop_sending(Code::H3_REQUEST_CANCELLED);
    drop(stream);
    sleep(Duration::from_millis(700)).await;
    let answer = daemon.get("/quick").await;
    assert_eq!((answer.status, answer.text().as_str()), (200, "quick"));
    assert_eq!(origin.requests().len(), 2);
    daemon.finish().await;
}
