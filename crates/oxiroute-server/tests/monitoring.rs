use std::thread;

use oxiroute_config::ListenerBind;
use oxiroute_server::{
    CertbotWatcherHealth, CertbotWatcherSnapshot, HttpOperationResult, MetricsError,
    RuntimeMetrics, TcpRelayResult,
};

#[test]
fn connection_guard_accounts_for_traffic_and_decrements_active_count() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("public-http", "http", "0.0.0.0:8080", 100)
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
        .register_listener("ingest", "rtmp", "127.0.0.1:1935", 100)
        .expect("listener registration");

    assert_eq!(listener.name(), "ingest");
    assert_eq!(listener.protocol(), "rtmp");
    assert_eq!(listener.bind(), "127.0.0.1:1935");
    assert!(matches!(
        metrics.register_listener("ingest", "tcp", "127.0.0.1:9000", 100),
        Err(MetricsError::DuplicateListener(name)) if name == "ingest"
    ));
    assert!(matches!(
        metrics.begin_connection("missing"),
        Err(MetricsError::ListenerNotFound(name)) if name == "missing"
    ));
}

#[test]
fn connection_limits_reject_excess_sessions_without_hiding_accepts() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("limited", "tcp", "127.0.0.1:7001", Some(1))
        .expect("listener registration");
    let connection = listener.begin_connection().expect("first connection");
    let traffic = listener.traffic_accounting();
    drop(traffic);

    assert!(matches!(
        listener.begin_connection(),
        Err(MetricsError::ConnectionLimitReached { listener, limit: 1 })
            if listener == "limited"
    ));
    let limited = metrics.snapshot().expect("limited snapshot");
    assert_eq!(limited.traffic.accepted_connections, 2);
    assert_eq!(limited.traffic.rejected_connections, 1);
    assert_eq!(limited.traffic.active_connections, 1);
    assert_eq!(limited.listeners[0].max_connections, Some(1));

    drop(connection);
    assert_eq!(
        metrics
            .snapshot()
            .expect("released snapshot")
            .traffic
            .active_connections,
        0
    );
}

#[test]
fn unbounded_connection_guards_never_apply_a_connection_cap() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("unbounded", "tcp", "127.0.0.1:7002", None)
        .expect("listener registration");

    let first = listener.begin_connection().expect("first connection");
    let second = listener.begin_connection().expect("second connection");
    second.record_bytes_received(11).expect("received bytes");
    first.record_bytes_sent(7).expect("sent bytes");

    let active = metrics.snapshot().expect("active snapshot");
    assert_eq!(active.traffic.accepted_connections, 2);
    assert_eq!(active.traffic.active_connections, 2);
    assert_eq!(active.traffic.bytes_received, 11);
    assert_eq!(active.traffic.bytes_sent, 7);
    assert_eq!(active.listeners[0].max_connections, None);

    drop((first, second));
    assert_eq!(
        metrics
            .snapshot()
            .expect("released snapshot")
            .traffic
            .active_connections,
        0
    );
}

#[test]
fn counters_are_shared_safely_across_threads() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("tcp", "tcp", "127.0.0.1:7000", 1_000)
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
    let listener = metrics
        .register_configured_listener(
            "http",
            "http",
            &ListenerBind::Socket {
                address: "[::]:8080".parse().expect("socket address"),
            },
            Some(100),
        )
        .expect("listener registration");
    listener
        .record_bytes_received(u64::MAX)
        .expect("maximum exact byte count");

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
    assert_eq!(json["traffic"]["acceptedConnections"], "0");
    assert_eq!(json["traffic"]["rejectedConnections"], "0");
    assert_eq!(json["traffic"]["activeConnections"], 0);
    assert_eq!(json["traffic"]["bytesReceived"], u64::MAX.to_string());
    assert_eq!(json["traffic"]["bytesSent"], "0");
    assert_eq!(json["listeners"][0]["name"], "http");
    assert_eq!(json["listeners"][0]["protocol"], "http");
    assert_eq!(json["listeners"][0]["bind"], "socket:[::]:8080");
    assert_eq!(json["listeners"][0]["maxConnections"], 100);
    assert_eq!(json["listeners"][0]["state"], "configured");
    assert_eq!(json["listeners"][0]["acceptedConnections"], "0");
    assert_eq!(json["listeners"][0]["rejectedConnections"], "0");
    assert_eq!(json["listeners"][0]["bytesReceived"], u64::MAX.to_string());
    assert_eq!(json["listeners"][0]["bytesSent"], "0");
    assert!(json["listeners"][0]["httpOperations"].is_null());
    assert!(json["listeners"][0]["tcpRelays"].is_null());
    assert_eq!(json["certbotCertificates"], serde_json::json!([]));
    assert!(json["certbotWatcher"].is_null());
}

#[test]
fn operation_snapshots_serialize_fixed_results_and_bounded_latency() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("edge", "http", "127.0.0.1:8080", None)
        .expect("listener registration");
    listener
        .record_http_operation(
            HttpOperationResult::ClientError,
            std::time::Duration::from_millis(7),
        )
        .expect("HTTP operation");
    listener
        .record_tcp_relay(
            TcpRelayResult::IdleTimeout,
            std::time::Duration::from_millis(65),
        )
        .expect("TCP relay");

    let json = serde_json::to_value(metrics.snapshot().expect("snapshot")).expect("JSON");

    assert_eq!(
        json["listeners"][0]["httpOperations"]["outcomes"][1]["result"],
        "client_error"
    );
    assert_eq!(
        json["listeners"][0]["httpOperations"]["outcomes"][1]["count"],
        "1"
    );
    assert_eq!(
        json["listeners"][0]["httpOperations"]["latency"]["count"],
        "1"
    );
    assert_eq!(
        json["listeners"][0]["httpOperations"]["latency"]["sumMs"],
        "7"
    );
    assert_eq!(
        json["listeners"][0]["tcpRelays"]["outcomes"][3]["result"],
        "idle_timeout"
    );
    assert_eq!(
        json["listeners"][0]["tcpRelays"]["latency"]["buckets"][5]["count"],
        "1"
    );
}

#[test]
fn certbot_watcher_counters_serialize_as_exact_decimal_strings() {
    let snapshot = CertbotWatcherSnapshot {
        health: CertbotWatcherHealth::Degraded,
        coalesced_events: u64::MAX,
        ignored_access_events: u64::MAX,
        backend_errors: u64::MAX,
        watch_recoveries: u64::MAX,
        watch_refreshes: u64::MAX,
        rescans: u64::MAX,
        periodic_rescans: u64::MAX,
        reconciliation_failures: u64::MAX,
    };

    let json = serde_json::to_value(snapshot).expect("Certbot watcher snapshot JSON");
    let exact = u64::MAX.to_string();
    for key in [
        "coalescedEvents",
        "ignoredAccessEvents",
        "backendErrors",
        "watchRecoveries",
        "watchRefreshes",
        "rescans",
        "periodicRescans",
        "reconciliationFailures",
    ] {
        assert_eq!(json[key], exact);
    }
}

#[test]
fn configured_bind_identities_distinguish_socket_and_unix_without_redaction() {
    let metrics = RuntimeMetrics::new();
    metrics
        .register_configured_listener(
            "socket",
            "http",
            &ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().expect("socket address"),
            },
            Some(10),
        )
        .expect("socket listener registration");
    metrics
        .register_configured_listener(
            "unix",
            "http",
            &ListenerBind::Unix {
                path: "/run/oxiroute/private-api.sock".into(),
                mode: None,
            },
            None,
        )
        .expect("Unix listener registration");

    let snapshot = metrics.snapshot().expect("listener snapshot");

    assert_eq!(snapshot.listeners[0].bind, "socket:127.0.0.1:8080");
    assert_eq!(
        snapshot.listeners[1].bind,
        "unix:/run/oxiroute/private-api.sock"
    );
    let json = serde_json::to_value(snapshot).expect("serialized snapshot");
    assert!(json["listeners"][1]["maxConnections"].is_null());
}
