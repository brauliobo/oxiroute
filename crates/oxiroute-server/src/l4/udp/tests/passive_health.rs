#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_connect_failure_records_one_passive_failure() {
    let harness = start_harness_with_options(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
        Some(passive_health(1, 1_000)),
        Some(SocketAddr::new(
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            9,
        )),
        None,
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    client
        .send_to(b"connect", harness.listener)
        .await
        .expect("connect failure datagram");
    wait_for_udp_passive_failures(&harness.generation, 1).await;
    wait_for_udp_references(&harness.generation, 0).await;

    let endpoint = &harness.generation.pools()[0].health_snapshot().endpoints[0];
    assert_eq!(endpoint.passive_failure_count, 1);
    assert_eq!(
        endpoint.passive_ejection_reason,
        Some(HealthFailure::ConnectFailed)
    );
    assert!(endpoint.passive_ejected);
    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_upstream_send_failure_is_passively_classified() {
    let peer = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("peer socket");
    let socket = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("upstream socket");
    socket
        .connect(peer.local_addr().expect("peer address"))
        .await
        .expect("connect upstream socket");
    let socket = socket.into_std().expect("convert upstream socket");
    rustix::net::shutdown(&socket, rustix::net::Shutdown::Both).expect("close upstream socket");
    let socket = UdpSocket::from_std(socket).expect("restore upstream socket");
    let (_shutdown_sender, mut shutdown) = watch::channel(false);

    let error = send_datagram(&socket, b"send-failure", None, &mut shutdown)
        .await
        .expect_err("closed upstream send");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(
        SessionEnd::UpstreamSend(error).passive_failure(),
        Some(HealthFailure::ProtocolError)
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_upstream_receive_failure_records_one_passive_failure() {
    let UdpHarness {
        listener,
        upstream,
        generation,
        shutdown,
        runtime,
    } = start_harness_with_options(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
        Some(passive_health(1, 1_000)),
        None,
        None,
    )
    .await;
    drop(upstream);
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    client
        .send_to(b"recv", listener)
        .await
        .expect("receive failure datagram");
    wait_for_udp_passive_failures(&generation, 1).await;
    wait_for_udp_references(&generation, 0).await;

    let endpoint = &generation.pools()[0].health_snapshot().endpoints[0];
    assert_eq!(endpoint.passive_failure_count, 1);
    assert_eq!(
        endpoint.passive_ejection_reason,
        Some(HealthFailure::ProtocolError)
    );
    shutdown.send(true).expect("shutdown UDP runtime");
    runtime.join().expect("join UDP runtime");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_oversized_reply_records_one_passive_failure() {
    let mut udp_policy = policy();
    udp_policy.max_datagram_bytes = 8;
    let harness = start_harness_with_options(
        udp_policy,
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
        Some(passive_health(1, 1_000)),
        None,
        None,
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    client
        .send_to(b"query", harness.listener)
        .await
        .expect("oversized reply datagram");
    let mut received = [0_u8; 32];
    let (_, peer) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("upstream query timeout")
    .expect("upstream query receive");
    harness
        .upstream
        .send_to(&[0; 9], peer)
        .await
        .expect("oversized upstream reply");
    wait_for_udp_passive_failures(&harness.generation, 1).await;
    wait_for_udp_references(&harness.generation, 0).await;
    assert!(
        timeout(Duration::from_millis(100), client.recv_from(&mut received))
            .await
            .is_err(),
        "oversized reply reached the client"
    );

    let endpoint = &harness.generation.pools()[0].health_snapshot().endpoints[0];
    assert_eq!(endpoint.passive_failure_count, 1);
    assert_eq!(
        endpoint.passive_ejection_reason,
        Some(HealthFailure::ProtocolError)
    );
    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_passive_failure_threshold_ejects_and_expiry_readmits() {
    let mut udp_policy = policy();
    udp_policy.max_datagram_bytes = 8;
    let harness = start_harness_with_options(
        udp_policy,
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
        Some(passive_health(2, 100)),
        None,
        None,
    )
    .await;
    let mut received = [0_u8; 32];
    for (failure_count, query) in [&b"first"[..], &b"second"[..]].into_iter().enumerate() {
        let client = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client socket");
        client
            .send_to(query, harness.listener)
            .await
            .expect("failure datagram");
        let (_, peer) = timeout(
            Duration::from_secs(2),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .expect("upstream query timeout")
        .expect("upstream query receive");
        harness
            .upstream
            .send_to(&[0; 9], peer)
            .await
            .expect("oversized upstream reply");
        wait_for_udp_passive_failures(
            &harness.generation,
            u64::try_from(failure_count + 1).expect("failure count"),
        )
        .await;
        wait_for_udp_references(&harness.generation, 0).await;
    }

    let ejected = &harness.generation.pools()[0].health_snapshot().endpoints[0];
    assert_eq!(ejected.passive_failure_count, 2);
    assert_eq!(ejected.passive_ejection_count, 1);
    assert!(ejected.passive_ejected);

    let rejected = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("rejected client socket");
    rejected
        .send_to(b"rejected", harness.listener)
        .await
        .expect("rejected datagram");
    assert!(
        timeout(
            Duration::from_millis(25),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .is_err(),
        "ejected endpoint received a new session"
    );

    sleep(Duration::from_millis(125)).await;
    let recovered = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("recovered client socket");
    recovered
        .send_to(b"recover", harness.listener)
        .await
        .expect("recovered datagram");
    let (length, peer) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("recovered query timeout")
    .expect("recovered query receive");
    assert_eq!(&received[..length], b"recover");
    harness
        .upstream
        .send_to(b"reply", peer)
        .await
        .expect("recovered reply");
    let (length, _) = timeout(Duration::from_secs(2), recovered.recv_from(&mut received))
        .await
        .expect("recovered reply timeout")
        .expect("recovered reply receive");
    assert_eq!(&received[..length], b"reply");
    assert!(!harness.generation.pools()[0].health_snapshot().endpoints[0].passive_ejected);
    harness.stop();
}
