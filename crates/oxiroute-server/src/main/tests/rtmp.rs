#[allow(clippy::wildcard_imports)]
use super::*;
use crate::test_ports::unused_tcp_address;

#[test]
fn rtmp_listener_admission_rolls_back_limits_and_retains_owned_drain_lifetime() {
    let directory = tempfile::tempdir().expect("RTMP generation directory");
    let recording_root = directory.path().join("recordings");
    fs::create_dir(&recording_root).expect("RTMP recording root");
    let listener_address = unused_tcp_address();
    let mut config = recorder_runtime_config(listener_address, &recording_root);
    config.max_connections = Some(1);
    config.listeners[0].max_connections = Some(1);
    let (_, _, generation) =
        activate_test_generation(&directory.path().join("oxiroute.kdl"), &config);
    let ServiceKind::Rtmp(service) = generation.services()[0].kind.clone() else {
        panic!("listener must compile as RTMP");
    };
    let metrics = generation
        .traffic_listener_metrics("live")
        .expect("RTMP listener metrics");
    let app = RtmpIngest::new(
        "live".into(),
        generation
            .rtmp_service(service.service_id())
            .expect("RTMP service runtime"),
        generation.rtmp_control(),
        service,
        metrics,
        Arc::clone(&generation),
    );

    assert!(app.accept_gate().is_some());
    let admission = app.admit_connection().expect("RTMP admission");
    assert_eq!(generation.active_references(RuntimeReferenceKind::Rtmp), 1);
    assert!(
        app.admit_connection().is_none(),
        "listener limit was bypassed"
    );
    assert_eq!(generation.active_references(RuntimeReferenceKind::Rtmp), 1);
    drop(admission);

    generation
        .metrics()
        .set_listener_administrative_state("live", oxiroute_server::AdministrativeState::Drain)
        .expect("drain RTMP listener");
    assert!(app.admit_connection().is_none());
    assert_eq!(generation.active_references(RuntimeReferenceKind::Rtmp), 0);
    generation
        .metrics()
        .set_listener_administrative_state("live", oxiroute_server::AdministrativeState::Ready)
        .expect("ready RTMP listener");

    generation.stop_accepting();
    assert!(!app.accepting());
    assert!(app.admit_connection().is_none());
    let owned = app
        .admit_owned_connection()
        .expect("accept-gate-owned RTMP admission");
    assert_eq!(generation.active_references(RuntimeReferenceKind::Rtmp), 1);
    assert!(!generation.drain(Duration::ZERO));
    let snapshot = generation.metrics().snapshot().expect("RTMP snapshot");
    assert_eq!(snapshot.traffic.accepted_connections, 3);
    assert_eq!(snapshot.traffic.active_connections, 1);
    assert!(snapshot.access_records.is_empty());

    drop(owned);
    assert!(generation.drain(Duration::from_millis(100)));
    assert_eq!(
        generation
            .metrics()
            .snapshot()
            .expect("released RTMP snapshot")
            .traffic
            .active_connections,
        0
    );
}

#[tokio::test]
async fn rtmp_writer_accounts_for_bytes_written_to_a_real_socket() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("RTMP writer bind");
    let address = listener.local_addr().expect("RTMP writer address");
    let client = tokio::spawn(async move {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("RTMP writer client");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.expect("RTMP bytes");
        bytes
    });
    let (server, _) = listener.accept().await.expect("RTMP writer accept");
    let mut server: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(server));
    let runtime = RuntimeMetrics::new();
    let metrics = runtime
        .register_listener("rtmp", "rtmp", address.to_string(), None)
        .expect("RTMP writer metrics");
    let packets = vec![b"first".to_vec(), b"-second".to_vec()];
    let mut counters = RtmpAccessCounters::default();

    write_rtmp_packets(&mut server, &packets, &metrics, &mut counters)
        .await
        .expect("RTMP packet write");
    drop(server);

    assert_eq!(client.await.expect("RTMP client task"), b"first-second");
    assert_eq!(
        runtime
            .snapshot()
            .expect("RTMP writer snapshot")
            .traffic
            .bytes_sent,
        12
    );
    assert_eq!(counters.bytes_sent, 12);
    assert_eq!(counters.messages_sent, 2);
}
