use std::thread;

use oxiroute_server::{MetricsError, RuntimeMetrics};

#[test]
fn connection_guard_accounts_for_traffic_and_decrements_active_count() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("public-http", "http", "0.0.0.0:8080")
        .expect("listener registration");

    let connection = listener.begin_connection().expect("accepted connection");
    connection
        .record_bytes_received(1_024)
        .expect("received bytes");
    connection.record_bytes_sent(2_048).expect("sent bytes");

    let active = metrics.snapshot().expect("active snapshot");
    assert_eq!(active.process.cpu_percent, None);
    assert_eq!(active.traffic.accepted_connections, 1);
    assert_eq!(active.traffic.active_connections, 1);
    assert_eq!(active.traffic.bytes_received, 1_024);
    assert_eq!(active.traffic.bytes_sent, 2_048);
    assert_eq!(active.listeners[0].name, "public-http");

    drop(connection);

    let closed = metrics.snapshot().expect("closed snapshot");
    assert_eq!(closed.traffic.accepted_connections, 1);
    assert_eq!(closed.traffic.active_connections, 0);
    assert_eq!(closed.listeners[0].active_connections, 0);
}

#[test]
fn registration_is_named_unique_and_listener_lookup_is_explicit() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("ingest", "rtmp", "127.0.0.1:1935")
        .expect("listener registration");

    assert_eq!(listener.name(), "ingest");
    assert_eq!(listener.protocol(), "rtmp");
    assert_eq!(listener.bind(), "127.0.0.1:1935");
    assert!(matches!(
        metrics.register_listener("ingest", "tcp", "127.0.0.1:9000"),
        Err(MetricsError::DuplicateListener(name)) if name == "ingest"
    ));
    assert!(matches!(
        metrics.begin_connection("missing"),
        Err(MetricsError::ListenerNotFound(name)) if name == "missing"
    ));
}

#[test]
fn counters_are_shared_safely_across_threads() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("tcp", "tcp", "127.0.0.1:7000")
        .expect("listener registration");
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let listener = listener.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    let connection = listener.begin_connection().expect("connection");
                    connection.record_bytes_received(1).expect("received byte");
                    connection.record_bytes_sent(2).expect("sent bytes");
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("metrics worker");
    }

    let snapshot = metrics.snapshot().expect("snapshot");
    assert_eq!(snapshot.traffic.accepted_connections, 400);
    assert_eq!(snapshot.traffic.active_connections, 0);
    assert_eq!(snapshot.traffic.bytes_received, 400);
    assert_eq!(snapshot.traffic.bytes_sent, 800);
}

#[test]
fn snapshot_serializes_to_the_monitoring_contract() {
    let metrics = RuntimeMetrics::new();
    metrics
        .register_listener("http", "http", "[::]:8080")
        .expect("listener registration");

    let snapshot = metrics.snapshot().expect("Linux process snapshot");
    let json = serde_json::to_value(snapshot).expect("serialized snapshot");

    assert!(json.get("sampledAtUnixMs").is_some());
    assert!(json.get("uptimeMs").is_some());
    assert!(json.get("sampled_at_unix_ms").is_none());
    assert!(json["process"]["cpuPercent"].is_null());
    assert!(json["process"]["residentMemoryBytes"].as_u64().is_some());
    assert!(json["process"]["virtualMemoryBytes"].as_u64().is_some());
    assert!(json["process"]["threadCount"].as_u64().is_some());
    assert!(json["process"]["openFileDescriptors"].as_u64().is_some());
    assert!(json["host"]["loadAverage1m"].as_f64().is_some());
    assert!(json["host"]["loadAverage5m"].as_f64().is_some());
    assert!(json["host"]["loadAverage15m"].as_f64().is_some());
    assert!(json["host"]["totalMemoryBytes"].as_u64().is_some());
    assert!(json["host"]["availableMemoryBytes"].as_u64().is_some());
    assert_eq!(json["traffic"]["acceptedConnections"], 0);
    assert_eq!(json["traffic"]["activeConnections"], 0);
    assert_eq!(json["traffic"]["bytesReceived"], 0);
    assert_eq!(json["traffic"]["bytesSent"], 0);
    assert_eq!(json["listeners"][0]["name"], "http");
    assert_eq!(json["listeners"][0]["protocol"], "http");
    assert_eq!(json["listeners"][0]["bind"], "[::]:8080");
}
