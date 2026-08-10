use std::{
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use pingora::{
    connectors::http::Connector as HttpConnector,
    http::RequestHeader,
    protocols::{ConnectionLifetime as _, http::client::HttpSession},
    upstreams::peer::HttpPeer,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

use super::*;

#[test]
fn normalized_nginx_hosts_accept_one_trailing_dot() {
    let authority = "API.EXAMPLE.TEST.:443"
        .parse::<Authority>()
        .expect("trailing-dot authority");

    assert_eq!(
        normalized_authority_host(&authority).as_deref(),
        Some("api.example.test")
    );
}

#[test]
fn dns_addresses_are_ordered_deterministically_for_every_consumer() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };
    let first = SocketAddr::from(([192, 0, 2, 1], 443));
    let second = SocketAddr::from(([192, 0, 2, 2], 443));

    let traffic_addresses = endpoint
        .order_addresses([second, first, second])
        .expect("traffic addresses");
    let health_addresses = endpoint
        .order_addresses([first, second])
        .expect("health addresses");

    assert_eq!(traffic_addresses, vec![first, second]);
    assert_eq!(health_addresses, traffic_addresses);
}

#[test]
fn dns_resolution_rejects_an_empty_address_set() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };

    let error = endpoint
        .order_addresses([])
        .expect_err("empty DNS resolution must fail");

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains("origin.example.test:443"));
}

#[test]
fn dns_resolution_accepts_the_normalized_address_limit() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };
    let expected = resolved_addresses(MAX_RESOLVED_ENDPOINT_ADDRESSES);
    let input = expected
        .iter()
        .rev()
        .copied()
        .chain(expected.iter().copied());

    let addresses = endpoint
        .order_addresses(input)
        .expect("address limit is accepted");

    assert_eq!(addresses, expected);
}

#[test]
fn dns_resolution_rejects_normalized_address_overflow() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };

    let error = endpoint
        .order_addresses(resolved_addresses(MAX_RESOLVED_ENDPOINT_ADDRESSES + 1))
        .expect_err("address overflow must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("returned more than 16 addresses")
    );
}

fn resolved_addresses(count: usize) -> Vec<SocketAddr> {
    (1..=count)
        .map(|index| {
            SocketAddr::from((
                [192, 0, 2, u8::try_from(index).expect("test address octet")],
                443,
            ))
        })
        .collect()
}

fn runtime_server(name: &str, port: u16, max_connections: Option<u64>) -> RuntimeServer {
    RuntimeServer {
        name: name.into(),
        endpoint: RuntimeEndpoint::from(SocketAddr::from(([127, 0, 0, 1], port))),
        max_connections,
        pinned_addresses: None,
        protected_addresses: Arc::from([]),
    }
}

fn connection_lifetime(pool: &EndpointPool, name: &str) -> Arc<EndpointLeaseInner> {
    let lease = pool
        .select_server_connection_target(name)
        .expect("connection target");
    Arc::clone(&lease.inner)
}

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

async fn wait_for_lifetime_waiters(pool: &EndpointPool, expected: u64) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.queue.lifetime_waiters.load(Ordering::Relaxed) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lifetime waiter count");
}

async fn wait_for_queued(pool: &EndpointPool, expected: u64) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued request count");
}

#[test]
fn administrative_drain_rejects_new_work_without_revoking_existing_leases() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, Some(2))],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    let lease = pool.select().expect("existing lease");

    pool.set_server_administrative_state("one", AdministrativeState::Drain)
        .expect("drain");

    assert!(pool.select().is_none());
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 1);
    drop(lease);
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
}

#[test]
fn maintenance_suspends_checks_while_drain_keeps_checks_running() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, None)],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");

    pool.set_server_administrative_state("one", AdministrativeState::Drain)
        .expect("drain");
    assert!(pool.health_checks_running(0));
    pool.set_server_administrative_state("one", AdministrativeState::Maintenance)
        .expect("maintenance");
    assert!(!pool.health_checks_running(0));
    pool.set_server_checks_enabled("one", false)
        .expect("disable checks");
    pool.set_server_administrative_state("one", AdministrativeState::Ready)
        .expect("ready");
    assert!(!pool.health_checks_running(0));
}

#[test]
fn health_override_is_independent_from_observed_health_and_resets_to_auto() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, None)],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
    assert!(pool.select().is_none());

    pool.set_server_health_override("one", HealthOverride::Up)
        .expect("force up");
    assert!(pool.select().is_some());
    let snapshot = pool.health_snapshot().endpoints.remove(0);
    assert_eq!(snapshot.state, EndpointHealthState::Unhealthy);
    assert_eq!(snapshot.health_override, HealthOverride::Up);

    pool.set_server_health_override("one", HealthOverride::Auto)
        .expect("automatic health");
    assert!(pool.select().is_none());
}

#[test]
fn max_connections_override_and_reset_preserve_configured_capacity() {
    let pool = RoundRobinPool::new_named_servers(
        "api".into(),
        [runtime_server("one", 3000, Some(2))],
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool");
    pool.set_server_max_connections("one", Some(1))
        .expect("override");
    let first = pool.select().expect("first");
    assert!(pool.select().is_none());
    drop(first);

    pool.set_server_max_connections("one", None).expect("reset");
    let first = pool.select().expect("first after reset");
    let second = pool.select().expect("configured second capacity");
    assert_eq!(pool.health_snapshot().endpoints[0].max_connections, Some(2));
    drop((first, second));
}

#[test]
fn first_uses_the_first_healthy_administrative_server_with_capacity() {
    let pool = RoundRobinPool::new_named_servers(
        "first".into(),
        [
            runtime_server("primary", 3000, Some(1)),
            runtime_server("backup", 3001, Some(1)),
        ],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("first pool");

    let primary = pool.select().expect("primary capacity");
    assert_eq!(primary.server_name(), "primary");
    let backup = pool.select().expect("backup capacity");
    assert_eq!(backup.server_name(), "backup");
    drop(primary);
    assert_eq!(
        pool.select().expect("primary restored").server_name(),
        "primary"
    );
    drop(backup);

    pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
    assert_eq!(
        pool.select().expect("healthy backup").server_name(),
        "backup"
    );
}

#[test]
fn least_connections_uses_named_server_work_and_rotates_equal_ties() {
    let pool = RoundRobinPool::new_named_servers(
        "least".into(),
        [
            runtime_server("one", 3000, Some(2)),
            runtime_server("two", 3001, Some(2)),
            runtime_server("three", 3002, Some(2)),
        ],
        UpstreamAlgorithm::LeastConnections,
        None,
        None,
    )
    .expect("least-connections pool");

    let one = pool.select().expect("one");
    let two = pool.select().expect("two");
    let three = pool.select().expect("three");
    assert_eq!(
        [one.server_name(), two.server_name(), three.server_name()],
        ["one", "two", "three"]
    );
    assert_eq!(
        pool.health_snapshot()
            .endpoints
            .iter()
            .map(|server| (server.name.as_str(), server.active_connections))
            .collect::<Vec<_>>(),
        vec![("one", 1), ("two", 1), ("three", 1)]
    );
}

#[tokio::test]
async fn bounded_capacity_queue_releases_and_times_out_exactly_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "queued".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_millis(30)),
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
    drop(held);
    let acquired = waiter
        .await
        .expect("waiter task")
        .expect("released capacity");
    assert_eq!(acquired.server_name(), "only");
    drop(acquired);
    let held = pool.select().expect("capacity after release");
    assert!(pool.select_wait().await.is_none());
    drop(held);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 1);
    assert_eq!(snapshot.queue_cancellations, 0);
    assert_eq!(snapshot.endpoints[0].active_connections, 0);
}

#[tokio::test]
async fn queue_disabled_selection_ignores_a_transient_fifo_registration() {
    let pool = RoundRobinPool::new_named_servers(
        "immediate".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("immediate pool");
    let _concurrent_front = QueueWaitGuard::new(Arc::clone(&pool.queue));

    let acquired = pool
        .select_wait()
        .await
        .expect("queue-disabled selection must attempt available capacity");

    drop(acquired);
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 0);
}

#[tokio::test]
async fn fifo_queue_sends_the_oldest_waiter_to_whichever_endpoint_frees_first() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "fifo".into(),
            [
                runtime_server("primary", 3000, Some(1)),
                runtime_server("secondary", 3001, Some(1)),
            ],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("FIFO pool"),
    );
    let primary = pool.select().expect("primary capacity");
    let secondary = pool.select().expect("secondary capacity");
    let old_pool = Arc::clone(&pool);
    let old = tokio::spawn(async move { old_pool.select_wait().await });
    wait_for_queued(&pool, 1).await;
    let new_pool = Arc::clone(&pool);
    let new = tokio::spawn(async move { new_pool.select_wait().await });
    wait_for_queued(&pool, 2).await;

    drop(secondary);
    let old = old
        .await
        .expect("old waiter task")
        .expect("old waiter lease");
    assert_eq!(old.server_name(), "secondary");
    wait_for_queued(&pool, 1).await;
    drop(primary);
    let new = new
        .await
        .expect("new waiter task")
        .expect("new waiter lease");
    assert_eq!(new.server_name(), "primary");
    drop((old, new));

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 0);
    assert_eq!(snapshot.queue_cancellations, 0);
}

#[tokio::test]
async fn cancelling_the_fifo_head_advances_the_next_waiter_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "fifo-cancel".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("FIFO cancellation pool"),
    );
    let held = pool.select().expect("held capacity");
    let old_pool = Arc::clone(&pool);
    let old = tokio::spawn(async move { old_pool.select_wait().await });
    wait_for_queued(&pool, 1).await;
    let new_pool = Arc::clone(&pool);
    let new = tokio::spawn(async move { new_pool.select_wait().await });
    wait_for_queued(&pool, 2).await;

    old.abort();
    assert!(old.await.expect_err("old waiter cancelled").is_cancelled());
    wait_for_queued(&pool, 1).await;
    drop(held);
    let acquired = new
        .await
        .expect("new waiter task")
        .expect("new waiter lease");
    drop(acquired);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 0);
    assert_eq!(snapshot.queue_cancellations, 1);
}

#[tokio::test]
async fn timing_out_the_fifo_head_exposes_free_capacity_to_the_next_waiter() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "fifo-timeout".into(),
            [
                runtime_server("primary", 3000, Some(1)),
                runtime_server("secondary", 3001, Some(1)),
            ],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_millis(100)),
        )
        .expect("FIFO timeout pool"),
    );
    let primary = pool.select().expect("primary capacity");
    let old_pool = Arc::clone(&pool);
    let old = tokio::spawn(async move { old_pool.select_server_wait("primary").await });
    wait_for_queued(&pool, 1).await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    let new_pool = Arc::clone(&pool);
    let new = tokio::spawn(async move { new_pool.select_wait().await });
    wait_for_queued(&pool, 2).await;

    assert!(old.await.expect("old waiter task").is_none());
    let acquired = new
        .await
        .expect("new waiter task")
        .expect("new waiter lease");
    assert_eq!(acquired.server_name(), "secondary");
    drop((primary, acquired));

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 2);
    assert_eq!(snapshot.queue_timeouts, 1);
    assert_eq!(snapshot.queue_cancellations, 0);
}

#[tokio::test]
async fn reusable_notification_is_skipped_when_queueing_is_immutably_disabled() {
    let pool = RoundRobinPool::new_named_servers(
        "unbounded".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("unbounded pool");
    let lifetime = connection_lifetime(&pool, "only");
    assert!(lifetime.try_acquire().expect("initial acquisition"));

    lifetime.notify_reusable();

    assert_eq!(pool.queue.generation.load(Ordering::Acquire), 0);
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 0);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn bounded_runtime_override_without_queueing_cannot_create_a_waiter() {
    let pool = RoundRobinPool::new_named_servers(
        "immediate".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("immediate pool");
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    pool.set_server_max_connections("only", Some(1))
        .expect("bounded override");
    let notifications = pool.queue.notifications.load(Ordering::Relaxed);
    let generation = pool.queue.generation.load(Ordering::Acquire);
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));

    assert!(second.wait_for_capacity(generation).await.is_err());
    first.notify_reusable();

    assert_eq!(
        pool.queue.notifications.load(Ordering::Relaxed),
        notifications
    );
    assert_eq!(pool.queue.generation.load(Ordering::Acquire), generation);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn reusable_notification_wakes_a_hidden_bounded_lifetime_waiter() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "bounded".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("bounded pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiting = Arc::clone(&second);
    let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;
    let notifications = pool.queue.notifications.load(Ordering::Relaxed);

    first.notify_reusable();

    waiter
        .await
        .expect("waiter task")
        .expect("reusable notification");
    assert_eq!(
        pool.queue.notifications.load(Ordering::Relaxed),
        notifications + 1
    );
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn capacity_overrides_preserve_wakes_in_both_directions() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "overrides".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("override pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("unbounded acquisition"));
    first.notify_reusable();
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);

    pool.set_server_max_connections("only", Some(1))
        .expect("None to Some override");
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 2);
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("bounded acquisition"));
    let generation = second.capacity_generation();
    let waiting = Arc::clone(&second);
    let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    pool.set_server_max_connections("only", None)
        .expect("Some to None override");

    waiter
        .await
        .expect("waiter task")
        .expect("override notification");
    assert!(second.try_acquire().expect("restored unbounded capacity"));
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 3);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn reusable_notification_before_waiter_registration_is_observed_by_generation() {
    let pool = RoundRobinPool::new_named_servers(
        "registration".into(),
        [runtime_server("only", 3000, Some(1))],
        UpstreamAlgorithm::First,
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("registration pool");
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();

    first.notify_reusable();

    tokio::time::timeout(
        Duration::from_millis(100),
        second.wait_for_capacity(generation),
    )
    .await
    .expect("generation change avoids a lost notification")
    .expect("generation notification");
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn cancelling_a_hidden_lifetime_waiter_releases_test_accounting_once() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "lifetime-cancel".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(10)),
        )
        .expect("cancellation pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiter = tokio::spawn(async move { second.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    waiter.abort();
    assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
    wait_for_lifetime_waiters(&pool, 0).await;
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn capacity_generation_wrap_still_wakes_registered_waiters() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "generation-wrap".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("generation pool"),
    );
    pool.queue.generation.store(u64::MAX, Ordering::Release);
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiter = tokio::spawn(async move { second.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    first.notify_reusable();

    waiter
        .await
        .expect("waiter task")
        .expect("wrapped generation notification");
    assert_eq!(pool.queue.generation.load(Ordering::Acquire), 0);
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), 1);
    assert_eq!(pool.queue.lifetime_waiters.load(Ordering::Relaxed), 0);
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
async fn public_selector_queue_counters_exclude_hidden_lifetime_waits() {
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
    assert_eq!(hidden_snapshot.queued, 0);
    assert_eq!(hidden_snapshot.queued_total, 0);
    assert_eq!(hidden_snapshot.queue_cancellations, 0);

    let selecting_pool = Arc::clone(&pool);
    let selector_waiter = tokio::spawn(async move { selecting_pool.select_wait().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("selector waiter registration");
    let combined_snapshot = pool.health_snapshot();
    assert_eq!(combined_snapshot.queued, 1);
    assert_eq!(combined_snapshot.queued_total, 1);
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
    assert_eq!(pool.health_snapshot().queue_cancellations, 0);

    selector_waiter.abort();
    assert!(
        selector_waiter
            .await
            .expect_err("selector waiter cancelled")
            .is_cancelled()
    );
    let final_snapshot = pool.health_snapshot();
    assert_eq!(final_snapshot.queued, 0);
    assert_eq!(final_snapshot.queued_total, 1);
    assert_eq!(final_snapshot.queue_cancellations, 1);
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

#[tokio::test]
async fn startup_dns_uses_only_the_addresses_pinned_in_the_runtime_plan() {
    let pinned = SocketAddr::from(([192, 0, 2, 10], 443));
    let pool = RoundRobinPool::new_named_servers(
        "pinned".into(),
        [RuntimeServer {
            name: "origin".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "origin.example.test".into(),
                port: 443,
            },
            max_connections: None,
            pinned_addresses: Some(vec![pinned].into()),
            protected_addresses: Arc::from([]),
        }],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("startup-pinned pool");

    let server = pool.select().expect("pinned server");
    assert_eq!(
        server.resolve_addresses().await.expect("pinned addresses"),
        vec![pinned]
    );
    assert_eq!(server.endpoint().to_string(), "origin.example.test:443");
}

#[tokio::test]
async fn dns_refresh_resolution_does_not_mutate_pinned_addresses_before_commit() {
    let pinned = SocketAddr::from(([192, 0, 2, 10], 443));
    let pool = RoundRobinPool::new_named_servers(
        "pinned".into(),
        [RuntimeServer {
            name: "origin".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "localhost".into(),
                port: 443,
            },
            max_connections: None,
            pinned_addresses: Some(vec![pinned].into()),
            protected_addresses: Arc::from([]),
        }],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("startup-pinned pool");

    let resolved = pool
        .resolve_server_dns("origin")
        .await
        .expect("external DNS resolution");
    let before_commit = pool.select().expect("server before commit");
    assert_eq!(
        before_commit
            .resolve_addresses()
            .await
            .expect("pinned address"),
        vec![pinned]
    );
    drop(before_commit);

    pool.commit_server_dns("origin", &resolved)
        .expect("atomic DNS commit");
    let after_commit = pool.select().expect("server after commit");
    assert_eq!(
        after_commit
            .resolve_addresses()
            .await
            .expect("committed addresses"),
        resolved
    );
}

#[tokio::test]
async fn on_connect_dns_rejects_a_protected_listener_after_resolution() {
    let protected = SocketAddr::from(([127, 0, 0, 1], 18404));
    let pool = RoundRobinPool::new_named_servers(
        "protected".into(),
        [RuntimeServer {
            name: "rebind".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "localhost".into(),
                port: protected.port(),
            },
            max_connections: None,
            pinned_addresses: None,
            protected_addresses: Arc::from([protected]),
        }],
        UpstreamAlgorithm::RoundRobin,
        None,
        None,
    )
    .expect("protected DNS pool");

    let lease = pool.select().expect("selected DNS server");
    let error = lease
        .resolve_addresses()
        .await
        .expect_err("protected address must be rejected after DNS resolution");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn checked_endpoints_transition_through_thresholds() {
    let pool = RoundRobinPool::new_named(
        "checked".into(),
        [RuntimeEndpoint::from(SocketAddr::from((
            [127, 0, 0, 1],
            3000,
        )))],
        UpstreamAlgorithm::RoundRobin,
        true,
    )
    .expect("checked pool");

    assert!(pool.select().is_none());
    assert_eq!(
        pool.health_snapshot().endpoints[0].state,
        EndpointHealthState::Unknown
    );

    pool.record_health(0, true, None, Some(100), 2, 2);
    assert!(
        !pool.has_available(),
        "one success remains below startup threshold"
    );
    pool.record_health(0, true, None, Some(150), 2, 2);
    assert!(
        pool.has_available(),
        "the startup threshold establishes state"
    );
    pool.record_health(
        0,
        false,
        Some(HealthFailure::ConnectFailed),
        Some(200),
        2,
        2,
    );
    assert!(pool.has_available(), "one failure remains below threshold");
    pool.record_health(
        0,
        false,
        Some(HealthFailure::ConnectFailed),
        Some(300),
        2,
        2,
    );
    assert!(
        !pool.has_available(),
        "the failure threshold removes the endpoint"
    );
    pool.record_health(0, true, None, Some(400), 2, 2);
    assert!(
        !pool.has_available(),
        "one success remains below recovery threshold"
    );
    pool.record_health(0, true, None, Some(500), 2, 2);
    assert!(
        pool.has_available(),
        "the recovery threshold restores the endpoint"
    );

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 1);
    assert_eq!(snapshot.endpoints[0].successful_checks, 4);
    assert_eq!(snapshot.endpoints[0].failed_checks, 2);
    assert_eq!(snapshot.endpoints[0].last_transition_at_unix_ms, Some(500));
    assert_eq!(snapshot.endpoints[0].last_failure, None);
}

#[test]
fn unchecked_endpoints_remain_selectable_without_observations() {
    let endpoint = SocketAddr::from(([127, 0, 0, 1], 3000));
    let pool = RoundRobinPool::new([endpoint]).expect("unchecked pool");

    assert_eq!(
        pool.select().map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(endpoint))
    );
    assert_eq!(
        pool.health_snapshot().endpoints[0].state,
        EndpointHealthState::Unchecked
    );
}

#[test]
fn excluding_every_available_endpoint_does_not_count_pool_unavailability() {
    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
    ];
    let pool = RoundRobinPool::new(endpoints).expect("unchecked pool");
    let endpoints = endpoints.map(RuntimeEndpoint::from);

    assert!(pool.select_excluding(&endpoints).is_none());
    assert_eq!(pool.health_snapshot().unavailable_selections, 0);
}

#[test]
fn selection_retries_across_a_pool_health_transition() {
    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
    ];
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "checked".into(),
            endpoints.map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool"),
    );
    pool.endpoints[0]
        .state
        .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
    pool.endpoints[1]
        .state
        .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
    let writer = pool.health.health_writer.lock().expect("health writer");
    pool.health.health_version.store(1, Ordering::Release);
    let barrier = Arc::new(Barrier::new(2));
    let (selection_tx, selection_rx) = mpsc::channel();
    let selection_pool = Arc::clone(&pool);
    let selection_barrier = Arc::clone(&barrier);
    let selection_task = thread::spawn(move || {
        selection_barrier.wait();
        selection_tx
            .send(
                selection_pool
                    .select()
                    .map(|lease| lease.endpoint().clone()),
            )
            .expect("selection receiver");
    });
    barrier.wait();
    assert!(
        selection_rx
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );

    pool.endpoints[0]
        .state
        .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
    pool.endpoints[1]
        .state
        .store(EndpointHealthState::Unhealthy as u8, Ordering::Relaxed);
    pool.health.health_version.store(2, Ordering::Release);
    drop(writer);

    assert_eq!(
        selection_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stable selection"),
        Some(RuntimeEndpoint::from(endpoints[0]))
    );
    selection_task.join().expect("selection task");
    assert_eq!(pool.health_snapshot().unavailable_selections, 0);
}

#[test]
fn health_aware_selection_is_fair_across_available_endpoints() {
    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
        SocketAddr::from(([127, 0, 0, 1], 3002)),
    ];
    let pool = RoundRobinPool::new_named(
        "checked".into(),
        endpoints.map(RuntimeEndpoint::from),
        UpstreamAlgorithm::RoundRobin,
        true,
    )
    .expect("checked pool");
    pool.record_health(
        0,
        false,
        Some(HealthFailure::ConnectFailed),
        Some(100),
        1,
        1,
    );
    pool.record_health(1, true, None, Some(100), 1, 1);
    pool.record_health(2, true, None, Some(100), 1, 1);

    let selected = (0..6)
        .map(|_| pool.select().map(|lease| lease.endpoint().clone()))
        .collect::<Vec<_>>();
    let endpoints = endpoints.map(RuntimeEndpoint::from);
    assert_eq!(
        selected,
        vec![
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
        ]
    );
}

#[test]
fn weighted_round_robin_is_deterministic_and_exposes_effective_weights() {
    let pool = RoundRobinPool::new_named_servers(
        "weighted".into(),
        [
            runtime_server("primary", 3000, None),
            runtime_server("backup", 3001, None),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1],
        },
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("weighted pool");

    let selected = (0..8)
        .map(|_| {
            pool.select()
                .expect("weighted endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        [
            "primary", "primary", "primary", "backup", "primary", "primary", "primary", "backup",
        ]
    );

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.algorithm, "weighted_round_robin");
    assert_eq!(
        snapshot
            .endpoints
            .iter()
            .map(|endpoint| endpoint.weight)
            .collect::<Vec<_>>(),
        vec![3, 1]
    );
}

#[test]
fn weighted_round_robin_skips_unhealthy_endpoints_and_recovers_them() {
    let pool = RoundRobinPool::new_named_servers(
        "weighted-health".into(),
        [
            runtime_server("primary", 3000, None),
            runtime_server("backup", 3001, None),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1],
        },
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("weighted health pool");
    pool.record_health(
        1,
        false,
        Some(HealthFailure::ConnectFailed),
        Some(100),
        1,
        1,
    );

    let unavailable = (0..4)
        .map(|_| {
            pool.select()
                .expect("healthy endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(unavailable, ["primary", "primary", "primary", "primary"]);
    assert_eq!(
        pool.health_snapshot().endpoints[1].state,
        EndpointHealthState::Unhealthy
    );

    pool.record_health(1, true, None, Some(200), 1, 1);
    let recovered = (0..4)
        .map(|_| {
            pool.select()
                .expect("recovered endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered, ["primary", "primary", "backup", "primary"]);
    assert_eq!(
        pool.health_snapshot().endpoints[1].state,
        EndpointHealthState::Healthy
    );
}

#[test]
fn unconfigured_passive_health_never_counts_or_ejects_failures() {
    let pool = RoundRobinPool::new_named_servers(
        "no-passive-policy".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("pool without passive policy");

    for _ in 0..10 {
        pool.record_passive_failure_at(0, HealthFailure::Timeout, now_unix_ms());
    }

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.available_endpoints, 1);
    assert_eq!(snapshot.unavailable_selections, 0);
    assert_eq!(snapshot.endpoints[0].passive_failure_count, 0);
    assert_eq!(snapshot.endpoints[0].passive_ejection_count, 0);
    assert!(!snapshot.endpoints[0].passive_ejected);
    assert!(pool.select().is_some());
}

#[test]
fn passive_ejection_is_deterministic_and_active_health_reentry_preserves_weighting() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-weighted".into(),
        [
            runtime_server("primary", 3000, None),
            runtime_server("backup", 3001, None),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1],
        },
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(2, Duration::from_mins(1), Duration::from_mins(2)),
    )
    .expect("passive policy pool");

    let base = now_unix_ms();
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, base);
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, base + 1);
    let ejected = pool.health_snapshot();
    assert_eq!(ejected.available_endpoints, 1);
    assert!(ejected.endpoints[0].passive_ejected);
    assert_eq!(ejected.endpoints[0].passive_failure_count, 2);
    assert_eq!(ejected.endpoints[0].passive_ejection_count, 1);
    assert_eq!(
        ejected.endpoints[0].passive_ejection_reason,
        Some(HealthFailure::ConnectFailed)
    );
    assert_eq!(
        ejected.endpoints[0].passive_ejected_at_unix_ms,
        Some(base + 1)
    );
    assert_eq!(
        ejected.endpoints[0].passive_ejection_until_unix_ms,
        Some(base + 60_001)
    );

    let selected_while_ejected = (0..16)
        .map(|_| {
            pool.select()
                .expect("backup remains eligible")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(selected_while_ejected.iter().all(|name| name == "backup"));

    pool.record_health(0, true, None, Some(base + 10), 1, 1);
    let recovered = pool.health_snapshot();
    assert!(!recovered.endpoints[0].passive_ejected);
    assert_eq!(recovered.endpoints[0].passive_recovery_count, 1);
    assert_eq!(
        recovered.endpoints[0].passive_last_recovery_at_unix_ms,
        Some(base + 10)
    );

    let selected_after_recovery = (0..4)
        .map(|_| {
            pool.select()
                .expect("recovered endpoint")
                .server_name()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_after_recovery,
        ["primary", "primary", "primary", "backup"]
    );
}

#[test]
fn passive_ejection_backoff_is_bounded_and_active_failures_are_not_double_counted() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-backoff".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_millis(100), Duration::from_millis(250)),
    )
    .expect("passive backoff pool");
    let base = now_unix_ms();

    pool.record_health(
        0,
        false,
        Some(HealthFailure::ConnectFailed),
        Some(base.saturating_sub(500)),
        1,
        1,
    );
    assert_eq!(pool.health_snapshot().endpoints[0].passive_failure_count, 0);

    pool.record_passive_failure_at(0, HealthFailure::Timeout, base);
    pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 101);
    pool.record_passive_failure_at(0, HealthFailure::Timeout, base + 302);
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.endpoints[0].passive_failure_count, 3);
    assert_eq!(snapshot.endpoints[0].passive_ejection_count, 3);
    assert_eq!(
        snapshot.endpoints[0].passive_ejection_until_unix_ms,
        Some(base + 552)
    );
    assert!(snapshot.endpoints[0].passive_ejected);
}

#[test]
fn passive_ejection_fails_closed_without_revoking_concurrent_leases() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-leases".into(),
        [runtime_server("only", 3000, Some(2))],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(1)),
    )
    .expect("passive lease pool");
    let base = now_unix_ms();
    let first = pool.select().expect("first lease");
    let second = pool.select().expect("second lease");

    pool.record_passive_failure_at(0, HealthFailure::ProtocolError, base);
    assert!(pool.select().is_none(), "ejected pool must fail closed");
    assert_eq!(pool.health_snapshot().unavailable_selections, 1);
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 2);

    drop((first, second));
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
}

#[test]
fn invalid_passive_policy_is_rejected_at_pool_construction() {
    let error = RoundRobinPool::new_named_servers_with_policy(
        "invalid-passive".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        None,
        None,
        PassiveFailurePolicy::new(0, Duration::from_secs(1), Duration::from_secs(1)),
    )
    .expect_err("zero threshold must be rejected");
    assert!(matches!(error, PoolError::InvalidPassivePolicy { .. }));
}

#[test]
fn passive_policy_bounds_are_rejected_at_pool_construction() {
    let policies = [
        PassiveFailurePolicy::new(101, Duration::from_secs(1), Duration::from_secs(1)),
        PassiveFailurePolicy::new(1, Duration::ZERO, Duration::from_secs(1)),
        PassiveFailurePolicy::new(1, Duration::from_secs(2), Duration::from_secs(1)),
        PassiveFailurePolicy::new(
            1,
            Duration::from_secs(1),
            Duration::from_secs(24 * 60 * 60 + 1),
        ),
    ];

    for policy in policies {
        let error = RoundRobinPool::new_named_servers_with_policy(
            "invalid-passive".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            None,
            None,
            policy,
        )
        .expect_err("invalid passive policy must be rejected");
        assert!(matches!(error, PoolError::InvalidPassivePolicy { .. }));
    }
}

#[test]
fn passive_ejection_expiry_allows_new_selection() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-expiry".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(1, Duration::from_millis(100), Duration::from_millis(100)),
    )
    .expect("passive expiry pool");
    let now = now_unix_ms();
    pool.record_passive_failure_at(0, HealthFailure::Timeout, now.saturating_sub(101));

    assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
    assert_eq!(
        pool.select().expect("expired endpoint").server_name(),
        "only"
    );
}

#[test]
fn configured_passive_observation_filters_layer_seven_failures() {
    let mut policy = PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy {
        observe: PassiveObserve::Layer4,
        on_error: PassiveOnError::Immediately,
        ..oxiroute_config::PassiveHealthPolicy::default()
    });
    policy.initial_ejection_duration = Duration::from_mins(1);
    policy.max_ejection_duration = Duration::from_mins(1);
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-observe".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("passive observe pool");

    pool.record_passive_failure_at(0, HealthFailure::ProtocolError, now_unix_ms());
    assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
    assert!(pool.health_snapshot().endpoints[0].passive_ejected);
}

#[test]
fn configured_mark_down_honors_the_error_limit_before_ejecting() {
    let policy = PassiveFailurePolicy::from_config(&oxiroute_config::PassiveHealthPolicy {
        on_error: PassiveOnError::MarkDown,
        error_limit: 2,
        ..oxiroute_config::PassiveHealthPolicy::default()
    });
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-mark-down".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("passive mark-down pool");

    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
    assert!(!pool.health_snapshot().endpoints[0].passive_ejected);
    pool.record_passive_failure_at(0, HealthFailure::ConnectFailed, now_unix_ms());
    let endpoint = &pool.health_snapshot().endpoints[0];
    assert!(endpoint.passive_ejected);
    assert_eq!(endpoint.state, EndpointHealthState::Unhealthy);
}

#[test]
fn weighted_round_robin_leases_respect_capacity_and_release() {
    let pool = RoundRobinPool::new_named_servers(
        "weighted-capacity".into(),
        [
            runtime_server("primary", 3000, Some(1)),
            runtime_server("backup", 3001, Some(1)),
        ],
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![2, 1],
        },
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("weighted capacity pool");

    let primary = pool.select().expect("primary lease");
    assert_eq!(primary.server_name(), "primary");
    let backup = pool.select().expect("backup lease");
    assert_eq!(backup.server_name(), "backup");
    assert!(pool.select().is_none(), "both endpoints are at capacity");
    drop(primary);
    assert_eq!(
        pool.select().expect("released primary").server_name(),
        "primary"
    );
    drop(backup);
}

#[test]
fn concurrent_health_aware_selection_distributes_every_available_turn() {
    const THREADS: usize = 8;
    const SELECTIONS_PER_THREAD: usize = 250;

    let endpoints = [
        SocketAddr::from(([127, 0, 0, 1], 3000)),
        SocketAddr::from(([127, 0, 0, 1], 3001)),
        SocketAddr::from(([127, 0, 0, 1], 3002)),
    ];
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "checked".into(),
            endpoints.map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool"),
    );
    pool.record_health(
        0,
        false,
        Some(HealthFailure::ConnectFailed),
        Some(100),
        1,
        1,
    );
    pool.record_health(1, true, None, Some(100), 1, 1);
    pool.record_health(2, true, None, Some(100), 1, 1);

    let selected = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                (0..SELECTIONS_PER_THREAD)
                    .map(|_| {
                        pool.select()
                            .expect("available endpoint")
                            .endpoint()
                            .clone()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|task| task.join().expect("selection thread"))
        .collect::<Vec<_>>();

    let endpoints = endpoints.map(RuntimeEndpoint::from);
    assert!(!selected.contains(&endpoints[0]));
    for endpoint in &endpoints[1..] {
        assert_eq!(
            selected
                .iter()
                .filter(|selected| *selected == endpoint)
                .count(),
            THREADS * SELECTIONS_PER_THREAD / 2
        );
    }
}

#[test]
fn concurrent_weighted_selection_preserves_cycle_fairness() {
    const THREADS: usize = 4;
    const SELECTIONS_PER_THREAD: usize = 100;

    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "weighted-concurrent".into(),
            [
                runtime_server("primary", 3000, None),
                runtime_server("backup", 3001, None),
            ],
            UpstreamAlgorithm::WeightedRoundRobin {
                weights: vec![3, 1],
            },
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("weighted concurrent pool"),
    );
    let selected = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                (0..SELECTIONS_PER_THREAD)
                    .map(|_| {
                        pool.select()
                            .expect("weighted endpoint")
                            .server_name()
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|task| task.join().expect("weighted selection thread"))
        .collect::<Vec<_>>();

    assert_eq!(
        selected
            .iter()
            .filter(|name| name.as_str() == "primary")
            .count(),
        THREADS * SELECTIONS_PER_THREAD * 3 / 4
    );
    assert_eq!(
        selected
            .iter()
            .filter(|name| name.as_str() == "backup")
            .count(),
        THREADS * SELECTIONS_PER_THREAD / 4
    );
}

#[test]
fn health_counters_serialize_without_losing_u64_precision() {
    let pool = RoundRobinPool::new_named(
        "checked".into(),
        [RuntimeEndpoint::from(SocketAddr::from((
            [127, 0, 0, 1],
            3000,
        )))],
        UpstreamAlgorithm::RoundRobin,
        true,
    )
    .expect("checked pool");
    pool.unavailable_selections
        .store(u64::MAX, Ordering::Relaxed);
    pool.endpoints[0]
        .active_work
        .store(u64::MAX, Ordering::Relaxed);
    pool.endpoints[0]
        .successful_checks
        .store(u64::MAX, Ordering::Relaxed);
    pool.endpoints[0]
        .failed_checks
        .store(u64::MAX, Ordering::Relaxed);
    pool.endpoints[0]
        .consecutive_successes
        .store(u64::MAX, Ordering::Relaxed);
    pool.endpoints[0]
        .consecutive_failures
        .store(u64::MAX, Ordering::Relaxed);

    let json = serde_json::to_value(pool.health_snapshot()).expect("health snapshot JSON");
    let exact = u64::MAX.to_string();
    assert_eq!(json["unavailableSelections"], exact);
    assert_eq!(json["endpoints"][0]["activeConnections"], exact);
    assert_eq!(json["endpoints"][0]["successfulChecks"], exact);
    assert_eq!(json["endpoints"][0]["failedChecks"], exact);
    assert_eq!(json["endpoints"][0]["consecutiveSuccesses"], exact);
    assert_eq!(json["endpoints"][0]["consecutiveFailures"], exact);
}

#[test]
fn health_snapshot_waits_for_a_complete_observation() {
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "checked".into(),
            [RuntimeEndpoint::from(SocketAddr::from((
                [127, 0, 0, 1],
                3000,
            )))],
            UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("checked pool"),
    );
    let writer = pool.health.health_writer.lock().expect("health writer");
    pool.health.health_version.store(1, Ordering::Release);
    let barrier = Arc::new(Barrier::new(2));
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let snapshot_pool = Arc::clone(&pool);
    let snapshot_barrier = Arc::clone(&barrier);
    let snapshot_task = thread::spawn(move || {
        snapshot_barrier.wait();
        snapshot_tx
            .send(snapshot_pool.health_snapshot())
            .expect("snapshot receiver");
    });
    barrier.wait();
    assert!(snapshot_rx.recv_timeout(Duration::from_millis(20)).is_err());

    pool.endpoints[0]
        .state
        .store(EndpointHealthState::Healthy as u8, Ordering::Relaxed);
    pool.endpoints[0]
        .last_checked_at_unix_ms
        .store(100, Ordering::Relaxed);
    pool.endpoints[0]
        .last_transition_at_unix_ms
        .store(100, Ordering::Relaxed);
    pool.endpoints[0]
        .successful_checks
        .store(1, Ordering::Relaxed);
    pool.health.health_version.store(2, Ordering::Release);
    drop(writer);

    let snapshot = snapshot_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("complete snapshot");
    snapshot_task.join().expect("snapshot task");
    assert_eq!(snapshot.endpoints[0].state, EndpointHealthState::Healthy);
    assert_eq!(snapshot.endpoints[0].last_checked_at_unix_ms, Some(100));
    assert_eq!(snapshot.endpoints[0].last_transition_at_unix_ms, Some(100));
    assert_eq!(snapshot.endpoints[0].successful_checks, 1);
}
