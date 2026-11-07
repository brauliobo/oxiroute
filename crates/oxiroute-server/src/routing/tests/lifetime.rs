use super::*;

fn peer_with_lifetime(
    address: SocketAddr,
    lifetime: &Arc<EndpointLeaseInner>,
    min_http_version: u8,
    max_http_version: u8,
) -> HttpPeer {
    let mut peer = HttpPeer::new(address, false, String::new());
    peer.options
        .set_http_version(max_http_version, min_http_version);
    let lifetime: Arc<dyn pingora::protocols::ConnectionLifetime> = lifetime.clone();
    peer.connection_lifetime = Some(Arc::downgrade(&lifetime));
    peer
}

async fn read_request_head(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.expect("request head");
        request.push(byte[0]);
    }
}

async fn acquire_after_capacity_change(lifetime: Arc<EndpointLeaseInner>) {
    loop {
        let generation = lifetime.capacity_generation();
        if lifetime.try_acquire().expect("capacity acquisition") {
            return;
        }
        lifetime
            .wait_for_capacity(generation)
            .await
            .expect("capacity notification");
    }
}

#[tokio::test]
async fn pingora_h1_hidden_waiter_wakes_when_the_connection_becomes_reusable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("H1 listener");
    let address = listener.local_addr().expect("H1 address");
    let (responded_tx, responded_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("H1 accept");
        read_request_head(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n")
            .await
            .expect("H1 response");
        responded_tx.send(()).expect("H1 response signal");
        let _ = finish_rx.await;
    });
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "pingora-h1".into(),
            [runtime_server("only", address.port(), Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("H1 pool"),
    );
    let first_lifetime = connection_lifetime(&pool, "only");
    let second_lifetime = connection_lifetime(&pool, "only");
    let first_peer = peer_with_lifetime(address, &first_lifetime, 1, 1);
    let second_peer = peer_with_lifetime(address, &second_lifetime, 1, 1);
    let connector = Arc::new(HttpConnector::new(None));
    let (mut first_session, reused) = connector
        .get_http_session(&first_peer)
        .await
        .expect("first H1 session");
    assert!(!reused);
    let HttpSession::H1(first_h1) = &mut first_session else {
        panic!("expected H1 session");
    };
    let mut request = Box::new(RequestHeader::build("GET", b"/", None).expect("H1 request"));
    request.append_header("Host", "localhost").expect("H1 host");
    first_h1
        .write_request_header(request)
        .await
        .expect("H1 request write");
    first_h1.read_response().await.expect("H1 response read");
    first_h1.respect_keepalive();
    while first_h1
        .read_body_bytes()
        .await
        .expect("H1 response body")
        .is_some()
    {}
    responded_rx.await.expect("H1 origin response");
    let waiting_connector = Arc::clone(&connector);
    let waiter =
        tokio::spawn(async move { waiting_connector.get_http_session(&second_peer).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    connector
        .release_http_session(first_session, &first_peer, None)
        .await;

    let (second_session, reused) = waiter
        .await
        .expect("H1 waiter task")
        .expect("second H1 session");
    assert!(reused);
    assert!(matches!(second_session, HttpSession::H1(_)));
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    drop(second_session);
    finish_tx.send(()).expect("finish H1 origin");
    server.await.expect("H1 server task");
}

#[tokio::test]
async fn pingora_h2_hidden_waiter_wakes_when_a_stream_becomes_reusable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("H2 listener");
    let address = listener.local_addr().expect("H2 address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("H2 accept");
        let mut connection = h2::server::handshake(stream).await.expect("H2 handshake");
        while let Some(request) = connection.accept().await {
            let _ = request.expect("H2 request");
        }
    });
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "pingora-h2".into(),
            [runtime_server("only", address.port(), Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("H2 pool"),
    );
    let first_lifetime = connection_lifetime(&pool, "only");
    let second_lifetime = connection_lifetime(&pool, "only");
    let mut first_peer = peer_with_lifetime(address, &first_lifetime, 2, 2);
    first_peer.options.max_h2_streams = 1;
    let mut second_peer = peer_with_lifetime(address, &second_lifetime, 2, 2);
    second_peer.options.max_h2_streams = 1;
    let connector = Arc::new(HttpConnector::new(None));
    let (first_session, reused) = connector
        .get_http_session(&first_peer)
        .await
        .expect("first H2 session");
    assert!(!reused);
    assert!(matches!(first_session, HttpSession::H2(_)));
    let waiting_connector = Arc::clone(&connector);
    let waiter =
        tokio::spawn(async move { waiting_connector.get_http_session(&second_peer).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    connector
        .release_http_session(first_session, &first_peer, None)
        .await;

    let (second_session, reused) = waiter
        .await
        .expect("H2 waiter task")
        .expect("second H2 session");
    assert!(reused);
    assert!(matches!(second_session, HttpSession::H2(_)));
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    drop(second_session);
    server.abort();
    assert!(
        server
            .await
            .expect_err("H2 server cancelled")
            .is_cancelled()
    );
}

#[tokio::test]
async fn multiple_hidden_waiters_survive_runtime_override_churn() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "override-churn".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("override churn pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("unbounded acquisition"));
    pool.set_server_max_connections("only", Some(1))
        .expect("None to Some override");
    let lifetimes = (0..3)
        .map(|_| connection_lifetime(&pool, "only"))
        .collect::<Vec<_>>();
    let waiters = lifetimes
        .iter()
        .map(|lifetime| tokio::spawn(acquire_after_capacity_change(Arc::clone(lifetime))))
        .collect::<Vec<_>>();
    wait_for_lifetime_waiters(&pool, 3).await;

    pool.set_server_max_connections("only", Some(2))
        .expect("first bounded override");
    wait_for_lifetime_waiters(&pool, 2).await;
    pool.set_server_max_connections("only", Some(3))
        .expect("second bounded override");
    wait_for_lifetime_waiters(&pool, 1).await;
    pool.set_server_max_connections("only", None)
        .expect("restore unbounded capacity");

    for waiter in waiters {
        waiter.await.expect("override waiter task");
    }
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 4);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 4);
    drop((first, lifetimes));
}

#[test]
fn reload_keeps_each_old_connection_on_its_original_queue_timeout_invariant() {
    let old_without_queue = RoundRobinPool::new_named_servers(
        "old-immediate".into(),
        [runtime_server("only", 3000, Some(1))],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("old immediate pool");
    let new_with_queue = RoundRobinPool::new_named_servers(
        "new-queued".into(),
        [runtime_server("only", 3000, Some(1))],
        UpstreamAlgorithm::First,
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("new queued pool");
    let old_immediate = connection_lifetime(&old_without_queue, "only");
    let new_queued = connection_lifetime(&new_with_queue, "only");

    old_immediate.notify_reusable();
    new_queued.notify_reusable();

    assert_eq!(
        old_without_queue
            .queue
            .notifications
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        new_with_queue.queue.notifications.load(Ordering::Relaxed),
        1
    );

    let old_with_queue = RoundRobinPool::new_named_servers(
        "old-queued".into(),
        [runtime_server("only", 3001, None)],
        UpstreamAlgorithm::First,
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("old queued pool");
    let new_without_queue = RoundRobinPool::new_named_servers(
        "new-immediate".into(),
        [runtime_server("only", 3001, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("new immediate pool");
    let old_queued = connection_lifetime(&old_with_queue, "only");
    let new_immediate = connection_lifetime(&new_without_queue, "only");

    old_queued.notify_reusable();
    new_immediate.notify_reusable();

    assert_eq!(
        old_with_queue.queue.notifications.load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        new_without_queue
            .queue
            .notifications
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn selector_and_bound_lifetime_waits_share_the_queue_counters() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "counter-ownership".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(10)),
        )
        .expect("counter pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let hidden = connection_lifetime(&pool, "only");
    assert!(!hidden.try_acquire().expect("hidden saturation"));
    let generation = hidden.capacity_generation();
    let hidden_waiter = tokio::spawn(async move { hidden.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;
    let hidden_snapshot = pool.health_snapshot();
    assert_eq!(hidden_snapshot.queued, 1);
    assert_eq!(hidden_snapshot.queued_total, 1);
    assert_eq!(hidden_snapshot.queue_cancellations, 0);

    let selecting_pool = Arc::clone(&pool);
    let selector_waiter = tokio::spawn(async move { selecting_pool.select_wait().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("selector waiter registration");
    let combined_snapshot = pool.health_snapshot();
    assert_eq!(combined_snapshot.queued, 2);
    assert_eq!(combined_snapshot.queued_total, 2);
    assert_eq!(combined_snapshot.queue_cancellations, 0);

    hidden_waiter.abort();
    assert!(
        hidden_waiter
            .await
            .expect_err("hidden waiter cancelled")
            .is_cancelled()
    );
    wait_for_lifetime_waiters(&pool, 0).await;
    assert_eq!(pool.health_snapshot().queued, 1);
    assert_eq!(pool.health_snapshot().queue_cancellations, 1);

    selector_waiter.abort();
    assert!(
        selector_waiter
            .await
            .expect_err("selector waiter cancelled")
            .is_cancelled()
    );
    let final_snapshot = pool.health_snapshot();
    assert_eq!(final_snapshot.queued, 0);
    assert_eq!(final_snapshot.queued_total, 2);
    assert_eq!(final_snapshot.queue_cancellations, 2);
}

#[tokio::test]
async fn capacity_release_cannot_race_a_waiter_notification_registration() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "wakeups".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("queued pool"),
    );

    for _ in 0..256 {
        let held = pool.select().expect("held capacity");
        let waiting_pool = Arc::clone(&pool);
        let waiter = tokio::spawn(async move { waiting_pool.select_wait().await });
        tokio::task::yield_now().await;
        drop(held);
        let acquired = tokio::time::timeout(Duration::from_millis(100), waiter)
            .await
            .expect("capacity notification was not lost")
            .expect("waiter task")
            .expect("released capacity");
        drop(acquired);
    }
    assert_eq!(pool.health_snapshot().queued, 0);
}

#[tokio::test]
async fn cancelling_a_capacity_waiter_rolls_back_queue_state_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "cancelled".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(10)),
        )
        .expect("queued pool"),
    );
    let held = pool.select().expect("initial capacity");
    let waiting_pool = Arc::clone(&pool);
    let waiter = tokio::spawn(async move { waiting_pool.select_wait().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiter entered queue");
    waiter.abort();
    assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
    drop(held);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 1);
    assert_eq!(snapshot.queue_cancellations, 1);
    assert_eq!(snapshot.queue_timeouts, 0);
    assert_eq!(snapshot.endpoints[0].active_connections, 0);
}
