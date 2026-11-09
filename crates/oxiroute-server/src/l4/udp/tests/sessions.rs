#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_routes_replies_and_releases_its_traffic_lease() {
    let upstream = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("upstream socket");
    let upstream_address = upstream.local_addr().expect("upstream address");
    let listener_address = StdUdpSocket::bind(("127.0.0.1", 0))
        .expect("listener probe")
        .local_addr()
        .expect("listener address");
    let config = udp_config(listener_address, upstream_address);
    let manager = GenerationManager::new();
    let candidate = manager
        .prepare(document(config))
        .expect("prepare UDP generation");
    let generation = manager
        .activate(&candidate)
        .expect("activate UDP generation");
    generation
        .metrics()
        .register_configured_listener(
            "relay",
            "udp",
            &ListenerBind::Udp {
                address: listener_address,
            },
            Some(8),
        )
        .expect("register UDP listener metrics");
    let service = match &generation.services()[0].kind {
        ServiceKind::Udp(service) => Arc::clone(service),
        _ => panic!("test service must be UDP"),
    };
    let reservation = generation
        .reservations()
        .get("relay")
        .cloned()
        .expect("UDP reservation");
    let metrics = generation
        .metrics()
        .listener("relay")
        .expect("listener metrics registry")
        .expect("UDP listener metrics");
    let (shutdown_tx, shutdown) = watch::channel(false);
    let runtime = UdpRuntime::start(
        "relay".into(),
        reservation,
        service,
        Arc::clone(&generation),
        metrics,
        None,
        shutdown,
    )
    .expect("start UDP runtime");
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    client
        .send_to(b"query", listener_address)
        .await
        .expect("send query");

    let mut received = [0_u8; 128];
    let (length, upstream_peer) =
        timeout(Duration::from_secs(30), upstream.recv_from(&mut received))
            .await
            .expect("upstream receive timeout")
            .expect("upstream receive");
    assert_eq!(&received[..length], b"query");
    upstream
        .send_to(b"response", upstream_peer)
        .await
        .expect("send response");

    let (length, _) = timeout(Duration::from_secs(30), client.recv_from(&mut received))
        .await
        .expect("client receive timeout")
        .expect("client receive");
    assert_eq!(&received[..length], b"response");

    shutdown_tx.send(true).expect("shutdown UDP runtime");
    runtime.join().expect("join UDP runtime");
    assert_eq!(generation.active_references(RuntimeReferenceKind::Udp), 0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_datagrams_are_dropped_before_session_admission() {
    let mut policy = policy();
    policy.max_datagram_bytes = 4;
    let harness = start_harness(
        policy,
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");

    client
        .send_to(b"12345", harness.listener)
        .await
        .expect("oversized datagram");
    assert!(
        timeout(
            Duration::from_millis(100),
            harness.upstream.recv_from(&mut [0; 32])
        )
        .await
        .is_err(),
        "oversized datagram reached the upstream"
    );

    client
        .send_to(b"1234", harness.listener)
        .await
        .expect("bounded datagram");
    let mut received = [0; 32];
    let (length, _) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("bounded datagram timeout")
    .expect("bounded datagram receive");
    assert_eq!(&received[..length], b"1234");

    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_drop_and_terminal_counters_account_bounded_work() {
    let mut udp_policy = policy();
    udp_policy.max_datagram_bytes = 4;
    let harness = start_harness(
        udp_policy,
        Duration::from_millis(40),
        Some(Duration::from_secs(2)),
        8,
        None,
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    client
        .send_to(b"12345", harness.listener)
        .await
        .expect("oversized datagram");
    timeout(Duration::from_secs(2), async {
        loop {
            let stats = harness.runtime.stats();
            if stats.datagrams_received == 1 && stats.datagrams_dropped == 1 {
                break;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("UDP drop accounting timeout");

    client
        .send_to(b"1234", harness.listener)
        .await
        .expect("bounded datagram");
    timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut [0_u8; 32]),
    )
    .await
    .expect("bounded datagram upstream timeout")
    .expect("bounded datagram upstream receive");
    timeout(Duration::from_secs(2), async {
        loop {
            if harness.runtime.stats().sessions_failed == 1 {
                break;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("UDP terminal accounting timeout");

    assert_eq!(
        harness.runtime.stats(),
        UdpRelayStats {
            datagrams_received: 2,
            datagrams_dropped: 1,
            sessions_started: 1,
            sessions_completed: 0,
            sessions_failed: 1,
        }
    );
    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_session_table_rejects_new_clients_at_max_sessions() {
    let mut policy = policy();
    policy.max_sessions = 1;
    let harness = start_harness(
        policy,
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let first = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("first client socket");
    let second = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("second client socket");
    let mut received = [0; 32];

    first
        .send_to(b"first", harness.listener)
        .await
        .expect("first session datagram");
    let (length, first_peer) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("first session timeout")
    .expect("first session receive");
    assert_eq!(&received[..length], b"first");

    second
        .send_to(b"second", harness.listener)
        .await
        .expect("second session datagram");
    assert!(
        timeout(
            Duration::from_millis(100),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .is_err(),
        "table overflow reached the upstream"
    );

    first
        .send_to(b"again", harness.listener)
        .await
        .expect("existing session datagram");
    let (length, peer) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("existing session timeout")
    .expect("existing session receive");
    assert_eq!(&received[..length], b"again");
    assert_eq!(peer, first_peer);

    let listener = harness
        .generation
        .metrics()
        .snapshot()
        .expect("UDP metrics snapshot")
        .listeners
        .into_iter()
        .find(|listener| {
            listener.name == "relay" && listener.state == crate::ListenerRuntimeState::Listening
        })
        .expect("UDP listener snapshot");
    assert_eq!(listener.accepted_connections, 1);
    assert_eq!(listener.rejected_connections, 0);
    assert_eq!(listener.active_connections, 1);

    assert_eq!(udp_passive_failure_count(&harness.generation), 0);
    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_session_enforces_byte_cap_and_releases_table_entry() {
    let mut policy = policy();
    policy.max_session_bytes = 10;
    let harness = start_harness(
        policy,
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    let mut received = [0; 32];

    client
        .send_to(&[0; 6], harness.listener)
        .await
        .expect("initial datagram");
    let (length, _) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("initial datagram timeout")
    .expect("initial datagram receive");
    assert_eq!(length, 6);

    client
        .send_to(&[0; 5], harness.listener)
        .await
        .expect("over-limit datagram");
    assert!(
        timeout(
            Duration::from_millis(100),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .is_err(),
        "over-limit session datagram reached the upstream"
    );
    wait_for_udp_references(&harness.generation, 0).await;

    client
        .send_to(&[0; 3], harness.listener)
        .await
        .expect("new session datagram");
    let (length, peer) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("new session timeout")
    .expect("new session receive");
    assert_eq!(length, 3);

    harness
        .upstream
        .send_to(&[0; 8], peer)
        .await
        .expect("over-limit upstream datagram");
    assert!(
        timeout(Duration::from_millis(100), client.recv_from(&mut received))
            .await
            .is_err(),
        "over-limit upstream datagram reached the client"
    );
    wait_for_udp_references(&harness.generation, 0).await;
    assert_eq!(udp_passive_failure_count(&harness.generation), 0);

    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_sessions_expire_after_idle_and_lifetime_deadlines() {
    let idle_harness = start_harness(
        policy(),
        Duration::from_millis(40),
        Some(Duration::from_secs(2)),
        8,
        None,
    )
    .await;
    let idle_client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("idle client socket");
    idle_client
        .send_to(b"idle", idle_harness.listener)
        .await
        .expect("idle datagram");
    let mut received = [0; 32];
    timeout(
        Duration::from_secs(2),
        idle_harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("idle upstream timeout")
    .expect("idle upstream receive");
    wait_for_udp_references(&idle_harness.generation, 0).await;
    assert_eq!(udp_passive_failure_count(&idle_harness.generation), 0);
    idle_harness.stop();

    let lifetime_harness = start_harness(
        policy(),
        Duration::from_secs(2),
        Some(Duration::from_millis(80)),
        8,
        None,
    )
    .await;
    let lifetime_client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("lifetime client socket");
    lifetime_client
        .send_to(b"life", lifetime_harness.listener)
        .await
        .expect("lifetime datagram");
    timeout(
        Duration::from_secs(2),
        lifetime_harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("lifetime upstream timeout")
    .expect("lifetime upstream receive");
    sleep(Duration::from_millis(100)).await;
    wait_for_udp_references(&lifetime_harness.generation, 0).await;
    assert_eq!(udp_passive_failure_count(&lifetime_harness.generation), 0);
    lifetime_harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_replies_keep_client_affinity_without_cross_client_leakage() {
    let harness = start_harness(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let first = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("first client socket");
    let second = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("second client socket");
    let mut received = [0; 32];

    first
        .send_to(b"one", harness.listener)
        .await
        .expect("first query");
    second
        .send_to(b"two", harness.listener)
        .await
        .expect("second query");
    let mut peers = Vec::new();
    for _ in 0..2 {
        let (length, peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("upstream query timeout")
        .expect("upstream query receive");
        peers.push((received[..length].to_vec(), peer));
    }

    for (query, peer) in peers {
        let reply = if query == b"one" {
            b"r-one".as_slice()
        } else {
            b"r-two".as_slice()
        };
        harness
            .upstream
            .send_to(reply, peer)
            .await
            .expect("upstream reply");
    }

    let (length, _) = timeout(Duration::from_secs(30), first.recv_from(&mut received))
        .await
        .expect("first reply timeout")
        .expect("first reply receive");
    assert_eq!(&received[..length], b"r-one");
    let (length, _) = timeout(Duration::from_secs(30), second.recv_from(&mut received))
        .await
        .expect("second reply timeout")
        .expect("second reply receive");
    assert_eq!(&received[..length], b"r-two");
    assert!(
        timeout(Duration::from_millis(100), first.recv_from(&mut received))
            .await
            .is_err(),
        "first client received a second client's reply"
    );
    assert!(
        timeout(Duration::from_millis(100), second.recv_from(&mut received))
            .await
            .is_err(),
        "second client received a first client's reply"
    );

    harness.stop();
}
