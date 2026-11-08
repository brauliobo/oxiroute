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
    assert!(output.contains("oxiroute_generation_retired 0"));
    assert!(output.contains("oxiroute_generation_forced_close_total{reason=\"drain_deadline\"} 0"));
    assert!(output.contains("oxiroute_generation_forced_close_total{reason=\"shutdown\"} 0"));
    assert!(output.contains("oxiroute_generation_retired_cap_deferrals_total 0"));
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
    crate::observability::operational_event::emit_rtmp_access("publish", "accepted");
    crate::observability::operational_event::emit_certificate(
        "certificate_renewal",
        "failed",
        "example.invalid",
    );
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
fn exposition_counts_header_selector_choices_by_route_and_pool() {
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("edge", "http", "socket:192.0.2.10:private-port", None)
        .expect("listener");
    listener.record_pool_selection("0", "gpu-12");
    listener.record_pool_selection("0", "gpu-12");
    listener.record_pool_selection("0", "gpu-24");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains("oxiroute_route_pool_selections_total{route=\"0\",pool=\"gpu-12\"} 2"));
    assert!(output.contains("oxiroute_route_pool_selections_total{route=\"0\",pool=\"gpu-24\"} 1"));
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

#[test]
fn exposition_has_queue_wait_histogram_and_oldest_wait_per_pool() {
    let metrics = RuntimeMetrics::new();
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "queue-wait-series".into(),
            [RuntimeServer {
                name: "0".into(),
                endpoint: RuntimeEndpoint::from(
                    "127.0.0.1:3000"
                        .parse::<std::net::SocketAddr>()
                        .expect("endpoint"),
                ),
                max_connections: Some(1),
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            }],
            UpstreamAlgorithm::RoundRobin,
            None,
            Some(std::time::Duration::from_secs(1)),
        )
        .expect("pool"),
    );
    crate::observability::upstream_stats::upstream_stats()
        .pool("queue-wait-series")
        .record_queue_wait(std::time::Duration::from_millis(30));
    metrics
        .register_upstream_pools([Arc::clone(&pool)])
        .expect("upstream pools");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    for expected in [
        "oxiroute_pool_queue_wait_milliseconds_bucket{pool=\"queue-wait-series\",le=\"25\"} 0",
        "oxiroute_pool_queue_wait_milliseconds_bucket{pool=\"queue-wait-series\",le=\"50\"} 1",
        "oxiroute_pool_queue_wait_milliseconds_bucket{pool=\"queue-wait-series\",le=\"600000\"} 1",
        "oxiroute_pool_queue_wait_milliseconds_count{pool=\"queue-wait-series\"} 1",
        "oxiroute_pool_queue_wait_milliseconds_sum{pool=\"queue-wait-series\"} 30",
        "oxiroute_pool_queue_oldest_wait_seconds{pool=\"queue-wait-series\"} 0",
    ] {
        assert!(output.contains(expected), "missing {expected}");
    }
}

#[test]
fn exposition_has_per_server_responses_attempts_bytes_and_latency() {
    let metrics = RuntimeMetrics::new();
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "server-series".into(),
            [RuntimeServer {
                name: "gpu-1".into(),
                endpoint: RuntimeEndpoint::from(
                    "127.0.0.1:3000"
                        .parse::<std::net::SocketAddr>()
                        .expect("endpoint"),
                ),
                max_connections: Some(4),
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            }],
            UpstreamAlgorithm::RoundRobin,
            None,
            None,
        )
        .expect("pool"),
    );
    let stats =
        crate::observability::upstream_stats::upstream_stats().server("server-series", "gpu-1");
    stats.record_response(Some(200));
    stats.record_response(Some(503));
    stats.record_attempt(crate::observability::upstream_stats::AttemptOutcome::Timeout);
    stats.record_attempt(crate::observability::upstream_stats::AttemptOutcome::Evacuated);
    stats.record_evacuation(
        crate::observability::upstream_stats::EvacuationResult::Redispatched,
        2,
    );
    stats.record_evacuation(
        crate::observability::upstream_stats::EvacuationResult::LeftRunning,
        1,
    );
    stats.record_bytes(7, 11);
    stats.record_request_duration(std::time::Duration::from_millis(40));
    stats.record_header_duration(std::time::Duration::from_millis(8));
    metrics
        .register_upstream_pools([Arc::clone(&pool)])
        .expect("upstream pools");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    for expected in [
        "oxiroute_server_responses_total{pool=\"server-series\",server=\"gpu-1\",class=\"2xx\"} 1",
        "oxiroute_server_responses_total{pool=\"server-series\",server=\"gpu-1\",class=\"5xx\"} 1",
        "oxiroute_server_responses_total{pool=\"server-series\",server=\"gpu-1\",class=\"none\"} 0",
        "oxiroute_server_attempts_total{pool=\"server-series\",server=\"gpu-1\",outcome=\"timeout\"} 1",
        "oxiroute_server_attempts_total{pool=\"server-series\",server=\"gpu-1\",outcome=\"evacuated\"} 1",
        "oxiroute_evacuated_requests_total{pool=\"server-series\",server=\"gpu-1\",result=\"redispatched\"} 2",
        "oxiroute_evacuated_requests_total{pool=\"server-series\",server=\"gpu-1\",result=\"left_running\"} 1",
        "oxiroute_evacuated_requests_total{pool=\"server-series\",server=\"gpu-1\",result=\"forced\"} 0",
        "oxiroute_evacuated_requests_total{pool=\"server-series\",server=\"gpu-1\",result=\"failed\"} 0",
        "oxiroute_server_bytes_sent_total{pool=\"server-series\",server=\"gpu-1\"} 7",
        "oxiroute_server_bytes_received_total{pool=\"server-series\",server=\"gpu-1\"} 11",
        "oxiroute_server_max_connections{pool=\"server-series\",server=\"gpu-1\"} 4",
        "oxiroute_server_request_duration_milliseconds_bucket{pool=\"server-series\",server=\"gpu-1\",le=\"50\"} 1",
        "oxiroute_server_header_duration_milliseconds_count{pool=\"server-series\",server=\"gpu-1\"} 1",
    ] {
        assert!(output.contains(expected), "missing {expected}");
    }
}

#[test]
fn server_histograms_drop_out_above_the_server_limit_but_counters_stay() {
    let metrics = RuntimeMetrics::new();
    let servers = (0..=SERVER_HISTOGRAM_LIMIT).map(|index| RuntimeServer {
        name: format!("s{index}"),
        endpoint: RuntimeEndpoint::from(std::net::SocketAddr::from(([127, 0, 0, 1], 3000))),
        max_connections: None,
        pinned_addresses: None,
        protected_addresses: Arc::from([]),
    });
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "wide".into(),
            servers,
            UpstreamAlgorithm::RoundRobin,
            None,
            None,
        )
        .expect("wide pool"),
    );
    metrics
        .register_upstream_pools([pool])
        .expect("upstream pools");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(
        output.contains(
            "oxiroute_server_responses_total{pool=\"wide\",server=\"s0\",class=\"2xx\"} 0"
        )
    );
    assert!(!output.contains("oxiroute_server_request_duration_milliseconds"));
    assert!(!output.contains("oxiroute_server_header_duration_milliseconds"));
    assert!(output.lines().count() < 20_000);
}

#[test]
fn exposition_counts_live_requests_per_pool_and_flags_overdue_ones() {
    use crate::http::inflight::{InflightArrival, Phase, registry};

    let arrival = |id: &str, deadline: std::time::Duration| InflightArrival {
        request_id: id.into(),
        listener: "edge".into(),
        method: "GET".into(),
        host: None,
        path: "/".into(),
        client_ip: None,
        deadline,
        access_log: None,
    };
    let live = registry().register(arrival("prom-live", std::time::Duration::from_secs(30)));
    live.set_pool("inflight-series");
    let stuck = registry().register(arrival("prom-stuck", std::time::Duration::from_secs(30)));
    stuck.set_pool("inflight-series");
    stuck.set_phase(Phase::AwaitingHeaders);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(
        !registry()
            .report_stuck(std::time::Duration::from_millis(10))
            .is_empty()
    );
    let metrics = RuntimeMetrics::new();
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    assert!(output.contains("oxiroute_http_inflight_requests{pool=\"inflight-series\"} 2"));
    assert!(output.contains("oxiroute_http_inflight_oldest_age_seconds{pool=\"inflight-series\"}"));
    assert!(output.contains("oxiroute_http_inflight_overdue 0"));
    assert!(output.contains(
        "oxiroute_http_stuck_requests_total{pool=\"inflight-series\",phase=\"awaiting_headers\"} 1"
    ));
}

#[test]
fn registry_series_cover_servers_operations_expiries_and_leases() {
    let mut metrics = crate::registry::service::RegistryMetrics::default();
    metrics.servers.insert(("render".into(), "active"), 2);
    metrics.operations.insert(("put", "ok"), 5);
    metrics.expirations.insert("render".into(), 1);
    metrics.rejections.insert("rate_limited", 3);
    metrics
        .lease_remaining_ms
        .insert(("render".into(), "gpu\"1".into()), 12_500);
    metrics.revision = 9;

    let mut output = String::new();
    registry::render_registry(&mut output, &metrics).expect("exposition");

    for line in [
        "oxiroute_registry_servers{pool=\"render\",state=\"active\"} 2",
        "oxiroute_registry_operations_total{op=\"put\",result=\"ok\"} 5",
        "oxiroute_registry_expirations_total{pool=\"render\"} 1",
        "oxiroute_registry_rejected_total{reason=\"rate_limited\"} 3",
        "oxiroute_registry_lease_remaining_seconds{pool=\"render\",server=\"gpu\\\"1\"} 12.5",
        "oxiroute_registry_store_errors_total 0",
        "oxiroute_registry_revision 9",
    ] {
        assert!(output.contains(line), "missing `{line}` in\n{output}");
    }
}

#[test]
fn exposition_reports_the_canary_of_each_pool_that_declares_one() {
    use crate::routing::{CanaryResult, CanaryRun, CanaryState};

    let metrics = RuntimeMetrics::new();
    let server = |name: &str| RuntimeServer {
        name: name.into(),
        endpoint: RuntimeEndpoint::from(
            "127.0.0.1:3000"
                .parse::<std::net::SocketAddr>()
                .expect("endpoint"),
        ),
        max_connections: None,
        pinned_addresses: None,
        protected_addresses: Arc::from([]),
    };
    let pool = |name: &str| {
        Arc::new(
            RoundRobinPool::new_named_servers(
                name.into(),
                [server("0")],
                UpstreamAlgorithm::RoundRobin,
                None,
                None,
            )
            .expect("pool"),
        )
    };
    let (watched, plain) = (pool("canary-series"), pool("no-canary-series"));
    let state = Arc::new(CanaryState::new(std::time::Duration::from_secs(30)));
    watched.attach_canary(Arc::clone(&state));
    for result in [CanaryResult::Ok, CanaryResult::Failed] {
        state.complete(CanaryRun {
            result,
            reason: None,
            server: Some("0".into()),
            duration: std::time::Duration::from_millis(42),
        });
    }
    metrics
        .register_upstream_pools([watched, plain])
        .expect("upstream pools");
    let control = empty_rtmp_runtime().control();

    let output = render_prometheus_control(&metrics, &control, &GenerationManager::new())
        .expect("exposition");

    for expected in [
        "oxiroute_canary_success{pool=\"canary-series\"} 0",
        "oxiroute_canary_duration_milliseconds{pool=\"canary-series\"} 42",
        "oxiroute_canary_consecutive_failures{pool=\"canary-series\"} 1",
        "oxiroute_canary_runs_total{pool=\"canary-series\",result=\"ok\"} 1",
        "oxiroute_canary_runs_total{pool=\"canary-series\",result=\"failed\"} 1",
    ] {
        assert!(output.contains(expected), "missing {expected}");
    }
    assert!(
        output.contains("oxiroute_canary_last_success_timestamp_seconds{pool=\"canary-series\"} ")
    );
    assert!(!output.contains("oxiroute_canary_success{pool=\"no-canary-series\"}"));
}
