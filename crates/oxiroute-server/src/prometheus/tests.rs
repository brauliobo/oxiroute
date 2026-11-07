use std::sync::Arc;

use oxiroute_config::UpstreamAlgorithm;
use oxiroute_rtmp::{PreparedRtmpRuntimeSet, RtmpPrepareContext, RtmpPrepareMode, RtmpRuntimeSet};

use crate::routing::{PoolResiliencePolicy, RuntimeServer};
use crate::{HealthFailure, PassiveFailurePolicy, RoundRobinPool, RuntimeEndpoint};

use super::*;

fn empty_rtmp_runtime() -> RtmpRuntimeSet {
    PreparedRtmpRuntimeSet::prepare(
        [],
        &RtmpPrepareContext::new(RtmpPrepareMode::Activation, []),
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .expect("empty RTMP preparation")
    .start(std::time::Instant::now() + std::time::Duration::from_secs(1))
    .expect("empty RTMP runtime")
}

#[test]
fn exposition_contains_operational_families_without_listener_binds() {
    let metrics = RuntimeMetrics::new();
    metrics
        .register_listener(
            "public\"edge",
            "http",
            "socket:192.0.2.10:private-port",
            None,
        )
        .expect("listener");
    let control = empty_rtmp_runtime().control();
    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains("oxiroute_process_uptime_seconds"));
    assert!(output.contains("listener=\"public\\\"edge\""));
    assert!(output.contains("oxiroute_generation_activations_total"));
    assert!(output.contains("oxiroute_audit_component_state{state=\"memory\"} 1"));
    assert!(
        output.contains(
            "oxiroute_audit_operations_total{category=\"reload\",result=\"requested\"} 0"
        )
    );
    assert!(output.contains("oxiroute_rtmp_relay_dns_refresh_attempts_total 0"));
    assert!(output.contains("oxiroute_rtmp_relay_dns_refresh_successes_total 0"));
    assert!(output.contains("oxiroute_rtmp_relay_dns_refresh_failures_total 0"));
    assert!(!output.contains("192.0.2.10"));
    assert!(!output.contains("private-port"));
}

#[test]
fn exposition_preserves_public_listener_display_name_order() {
    let metrics = RuntimeMetrics::new();
    metrics
        .register_listener("zulu", "http", "socket:127.0.0.1:8080", None)
        .unwrap();
    metrics
        .register_listener("alpha", "http", "socket:127.0.0.1:8081", None)
        .unwrap();
    let control = empty_rtmp_runtime().control();
    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new()).unwrap();

    assert!(output.find("listener=\"alpha\"") < output.find("listener=\"zulu\""));
}

#[test]
fn exposition_contains_bounded_http_and_tcp_outcome_histograms() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("edge", "http", "socket:192.0.2.10:private-port", None)
        .expect("listener");
    listener
        .record_http_operation(
            crate::HttpOperationResult::Success,
            std::time::Duration::from_millis(7),
        )
        .expect("HTTP result");
    listener
        .record_http_operation(
            crate::HttpOperationResult::UpstreamError,
            std::time::Duration::from_millis(65),
        )
        .expect("HTTP result");
    listener
        .record_tcp_relay(
            crate::TcpRelayResult::IdleTimeout,
            std::time::Duration::from_secs(2),
        )
        .expect("TCP result");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(
        output.contains("oxiroute_http_requests_total{listener=\"edge\",result=\"success\"} 1")
    );
    assert!(
        output.contains(
            "oxiroute_http_requests_total{listener=\"edge\",result=\"upstream_error\"} 1"
        )
    );
    assert!(output.contains(
        "oxiroute_http_request_duration_milliseconds_bucket{listener=\"edge\",le=\"10\"} 1"
    ));
    assert!(
        output.contains("oxiroute_tcp_relays_total{listener=\"edge\",result=\"idle_timeout\"} 1")
    );
    assert!(output.contains(
        "oxiroute_tcp_relay_duration_milliseconds_bucket{listener=\"edge\",le=\"5000\"} 1"
    ));
    assert!(!output.contains("/sensitive?token="));
}

#[test]
fn exposition_uses_only_fixed_transport_outcome_labels() {
    let metrics = RuntimeMetrics::new();
    let forward = metrics
        .register_listener(
            "forward",
            "forward_http1",
            "socket:192.0.2.10:private-port",
            None,
        )
        .expect("forward listener");
    forward
        .record_http_operation(
            crate::HttpOperationResult::ClientError,
            std::time::Duration::from_millis(7),
        )
        .expect("forward result");
    for transport in [
        crate::ObservedTransport::Rtmp,
        crate::ObservedTransport::Forward,
        crate::ObservedTransport::Cache,
        crate::ObservedTransport::Udp,
        crate::ObservedTransport::H3,
        crate::ObservedTransport::Acme,
    ] {
        metrics
            .record_transport_operation(
                transport,
                crate::TransportOutcome::Timeout,
                std::time::Duration::from_millis(7),
            )
            .expect("transport result");
    }
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(
        output.contains(
            "oxiroute_transport_operations_total{transport=\"rtmp\",outcome=\"timeout\"} 1"
        )
    );
    assert!(output.contains(
        "oxiroute_transport_operations_total{transport=\"forward\",outcome=\"client_error\"} 1"
    ));
    assert!(output.contains(
        "oxiroute_transport_operation_duration_milliseconds_bucket{transport=\"h3\",le=\"10\"} 1"
    ));
    assert!(!output.contains("uri="));
    assert!(!output.contains("query="));
    assert!(!output.contains("body="));
    assert!(!output.contains("credential="));
}

#[test]
fn exposition_includes_bounded_rtmp_and_acme_event_outcomes() {
    crate::operational_event::emit_rtmp_access("publish", "accepted");
    crate::operational_event::emit_certificate("certificate_renewal", "failed", "example.invalid");
    let metrics = RuntimeMetrics::new();
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(
        output.contains("oxiroute_transport_events_total{transport=\"rtmp\",outcome=\"success\"}")
    );
    assert!(output.contains(
        "oxiroute_transport_events_total{transport=\"acme\",outcome=\"upstream_error\"}"
    ));
}

#[test]
fn exposition_includes_bounded_rtmp_access_log_counters_without_labels() {
    let metrics = RuntimeMetrics::new();
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains("oxiroute_rtmp_access_log_queue_capacity 1024"));
    assert!(output.contains("oxiroute_rtmp_access_log_queue_depth 0"));
    assert!(output.contains("oxiroute_rtmp_access_log_events_dropped_total"));
    assert!(output.contains("oxiroute_rtmp_access_log_queue_saturation_total"));
    assert!(output.contains("oxiroute_rtmp_access_log_write_failures_total"));
    assert!(!output.contains("sessionId="));
    assert!(!output.contains("path="));
}

#[test]
fn exposition_includes_retry_and_queue_totals() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("edge", "http", "socket:192.0.2.10:private-port", None)
        .expect("listener");
    listener.record_retry_attempt();
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains("oxiroute_upstream_retry_attempts_total 1"));
    assert!(!output.contains("192.0.2.10"));
}

#[test]
fn exposition_counts_rescues_by_route_and_status() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("edge", "http", "socket:192.0.2.10:private-port", None)
        .expect("listener");
    listener.record_rescue("ollama", 404);
    listener.record_rescue("ollama", 404);
    listener.record_rescue("ollama", 503);
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains("oxiroute_http_rescues_total{route=\"ollama\",status=\"404\"} 2"));
    assert!(output.contains("oxiroute_http_rescues_total{route=\"ollama\",status=\"503\"} 1"));
}

#[test]
fn exposition_contains_bounded_passive_endpoint_observability() {
    let metrics = RuntimeMetrics::new();
    let pool = Arc::new(
        RoundRobinPool::new_named_servers_resilient(
            "observability".into(),
            [RuntimeServer {
                name: "0".into(),
                endpoint: RuntimeEndpoint::from(
                    "127.0.0.1:3000"
                        .parse::<std::net::SocketAddr>()
                        .expect("endpoint"),
                ),
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            }],
            UpstreamAlgorithm::RoundRobin,
            None,
            None,
            PassiveFailurePolicy::new(
                3,
                std::time::Duration::from_secs(30),
                std::time::Duration::from_mins(5),
            ),
            PoolResiliencePolicy::from_config(&oxiroute_config::PoolResilience {
                fail_open: true,
                min_members: Some(0),
                ..oxiroute_config::PoolResilience::default()
            }),
        )
        .expect("pool"),
    );
    for _ in 0..3 {
        pool.record_passive_failure(0, HealthFailure::ConnectFailed);
    }
    assert!(pool.select().is_some(), "an ejected pool fails open");
    metrics
        .register_upstream_pools([Arc::clone(&pool)])
        .expect("upstream pools");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains(
            "oxiroute_server_passive_ejected{pool=\"observability\",server=\"0\",reason=\"connect_failed\"} 1"
        ));
    assert!(output.contains(
            "oxiroute_server_passive_failures_total{pool=\"observability\",server=\"0\",reason=\"connect_failed\"} 3"
        ));
    assert!(output.contains(
        "oxiroute_server_passive_ejections_total{pool=\"observability\",server=\"0\"} 1"
    ));
    assert!(output.contains("oxiroute_pool_fail_open_selections_total{pool=\"observability\"} 1"));
    assert!(output.contains("oxiroute_pool_queue_admissions_total{pool=\"observability\"} 0"));
    assert!(output.contains("oxiroute_pool_queue_cancellations_total{pool=\"observability\"} 0"));
    assert!(!output.contains("127.0.0.1:3000"));
}
