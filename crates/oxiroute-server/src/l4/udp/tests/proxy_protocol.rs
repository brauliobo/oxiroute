#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_proxy_v2_datagrams_are_rejected_before_upstream_delivery() {
    let harness = start_harness(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        Some(ProxyProtocolPolicy {
            version: ProxyProtocolVersion::V2,
            timeout_ms: 1_000,
        }),
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    client
        .send_to(
            &[0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x00, 0x00, 0x00],
            harness.listener,
        )
        .await
        .expect("malformed PROXY v2 datagram");

    assert!(
        timeout(
            Duration::from_millis(100),
            harness.upstream.recv_from(&mut [0; 32])
        )
        .await
        .is_err(),
        "malformed PROXY v2 datagram reached the upstream"
    );
    timeout(Duration::from_secs(2), async {
        while harness.runtime.stats().datagrams_dropped != 1 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("PROXY rejection accounting timeout");
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
    assert_eq!(listener.accepted_connections, 0);
    assert_eq!(listener.rejected_connections, 0);
    assert_eq!(listener.active_connections, 0);
    harness.stop();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_proxy_v2_is_accepted_and_propagated_on_the_wire() {
    let proxy_policy = ProxyProtocolPolicy {
        version: ProxyProtocolVersion::V2,
        timeout_ms: 1_000,
    };
    let harness = start_harness_with_options(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        Some(proxy_policy),
        None,
        None,
        Some(proxy_policy),
    )
    .await;
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    let source = SocketAddr::from(([192, 0, 2, 10], 1234));
    let mut datagram = encode_header(
        ProxyProtocolVersion::V2,
        ProxyProtocolTransport::Datagram,
        source,
        harness.listener,
    )
    .expect("client PROXY header");
    datagram.extend_from_slice(b"query");
    client
        .send_to(&datagram, harness.listener)
        .await
        .expect("PROXY query");

    let mut received = [0_u8; 128];
    let (length, upstream_peer) = timeout(
        Duration::from_secs(2),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("upstream PROXY query timeout")
    .expect("upstream PROXY query receive");
    let header = parse_header(
        &received[..length],
        ProxyProtocolVersion::V2,
        ProxyProtocolTransport::Datagram,
    )
    .expect("upstream PROXY parse")
    .expect("complete upstream PROXY header");
    assert_eq!(header.source, source);
    assert_eq!(
        header.destination,
        harness.upstream.local_addr().expect("upstream address")
    );
    assert_eq!(&received[header.consumed..length], b"query");

    harness
        .upstream
        .send_to(b"response", upstream_peer)
        .await
        .expect("upstream response");
    let (length, _) = timeout(Duration::from_secs(30), client.recv_from(&mut received))
        .await
        .expect("client response timeout")
        .expect("client response receive");
    assert_eq!(&received[..length], b"response");
    harness.stop();
}
