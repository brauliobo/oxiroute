#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_reload_rejects_new_sessions_and_shutdown_cancels_active_sessions() {
    let harness = start_harness(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let active = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("active client socket");
    let new_client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("new client socket");
    let mut received = [0; 32];

    active
        .send_to(b"active", harness.listener)
        .await
        .expect("active session datagram");
    timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("active session timeout")
    .expect("active session receive");
    harness.generation.stop_accepting();

    new_client
        .send_to(b"reload", harness.listener)
        .await
        .expect("reload datagram");
    assert!(
        timeout(
            Duration::from_millis(100),
            harness.upstream.recv_from(&mut received),
        )
        .await
        .is_err(),
        "reload admitted new UDP work"
    );
    sleep(Duration::from_millis(100)).await;
    assert!(
        !harness
            .runtime
            .thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished),
        "reload terminated the UDP listener before shutdown"
    );

    harness.shutdown.send(true).expect("shutdown UDP runtime");
    harness.runtime.join().expect("join UDP runtime");
    assert_eq!(udp_passive_failure_count(&harness.generation), 0);
    assert_eq!(
        harness
            .generation
            .active_references(RuntimeReferenceKind::Udp),
        0
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_administrative_drain_does_not_record_active_upstream_failure() {
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
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    let mut received = [0_u8; 32];
    client
        .send_to(b"active", listener)
        .await
        .expect("active datagram");
    timeout(Duration::from_secs(2), upstream.recv_from(&mut received))
        .await
        .expect("active upstream timeout")
        .expect("active upstream receive");
    generation.stop_accepting();
    drop(upstream);
    client
        .send_to(b"again", listener)
        .await
        .expect("draining datagram");
    wait_for_udp_references(&generation, 0).await;
    assert_eq!(udp_passive_failure_count(&generation), 0);
    shutdown.send(true).expect("shutdown UDP runtime");
    runtime.join().expect("join UDP runtime");
}
