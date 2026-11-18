use oxiroute_config::ConfigDraft;

use super::*;

fn generation() -> std::sync::Arc<crate::RuntimeGeneration> {
    let config = ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        http_data_plane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        worker_scaling: None,
        min_active_workers: None,
        target_busy_percent: None,
        scale_down_delay_ms: None,
        tls_handshake_threads: None,
        http3_congestion_control: None,
        http3_segmentation_offload: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
        audit: None,
        config_history: None,
        log_filter: None,
    };
    let manager = crate::GenerationManager::new();
    let candidate = manager
        .prepare(crate::config::coordinator::ResolvedConfigDocument {
            authored_revision: crate::config::coordinator::AuthoredRevision::from_bytes(
                b"listener-runtime-test",
            ),
            effective_revision: crate::config::coordinator::EffectiveRevision::from_bytes(
                b"listener-runtime-test",
            ),
            validated_config: config.validate().expect("valid generation config"),
            format: oxiroute_config_source::ConfigFormat::Lua,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: String::new(),
            diagnostics: Vec::new(),
        })
        .expect("generation candidate");
    manager.activate(&candidate).expect("active generation")
}

#[test]
fn listener_admission_rolls_back_on_metrics_rejection_and_drains_after_release() {
    let generation = generation();
    let metrics = crate::RuntimeMetrics::with_max_connections(Some(1));
    let listener = metrics
        .register_listener("edge", "forward_http1", "127.0.0.1:8080", Some(1))
        .expect("listener");
    let runtime = ListenerRuntime::new(listener);

    metrics
        .set_listener_administrative_state("edge", crate::AdministrativeState::Drain)
        .expect("listener drain");
    assert!(
        runtime
            .admit(&generation, crate::RuntimeReferenceKind::ForwardHttp1)
            .is_err()
    );
    assert_eq!(
        generation.active_references(crate::RuntimeReferenceKind::ForwardHttp1),
        0
    );
    metrics
        .set_listener_administrative_state("edge", crate::AdministrativeState::Ready)
        .expect("listener ready");

    let held = runtime
        .admit_owned(&generation, crate::RuntimeReferenceKind::ForwardHttp1)
        .expect("held listener admission");

    assert!(
        runtime
            .admit(&generation, crate::RuntimeReferenceKind::ForwardHttp1)
            .is_err()
    );
    assert_eq!(
        generation.active_references(crate::RuntimeReferenceKind::ForwardHttp1),
        1
    );
    generation.stop_accepting();
    assert!(!generation.drain(std::time::Duration::ZERO));

    drop(held);
    assert!(generation.drain(std::time::Duration::from_millis(100)));
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
fn control_admission_rolls_back_and_bypasses_only_process_drain() {
    let generation = generation();
    let metrics = crate::RuntimeMetrics::with_max_connections(Some(1));
    let listener = metrics
        .register_listener("management", "http", "127.0.0.1:8080", Some(1))
        .expect("listener");
    let runtime = ListenerRuntime::new(listener);

    metrics
        .set_listener_administrative_state("management", crate::AdministrativeState::Drain)
        .expect("listener drain");
    assert!(runtime.admit_control(&generation).is_err());
    assert!(generation.drained());

    metrics
        .set_listener_administrative_state("management", crate::AdministrativeState::Ready)
        .expect("listener ready");
    metrics.set_process_administrative_state(crate::AdministrativeState::Drain);
    let lease = runtime
        .admit_control(&generation)
        .expect("control admission during process drain");
    generation.stop_accepting();
    assert!(!generation.drain(std::time::Duration::ZERO));

    drop(lease);
    assert!(generation.drain(std::time::Duration::from_millis(100)));
    let snapshot = metrics.snapshot().expect("released snapshot");
    assert_eq!(snapshot.process.active_connections, 0);
    assert_eq!(snapshot.listeners[0].active_connections, 0);
    assert!(snapshot.access_records.is_empty());
}

#[test]
fn control_admission_outlives_the_traffic_drain_and_ends_with_the_generation() {
    let generation = generation();
    let metrics = crate::RuntimeMetrics::with_max_connections(None);
    let traffic = ListenerRuntime::new(
        metrics
            .register_listener("web", "http", "127.0.0.1:8080", None)
            .expect("traffic listener"),
    );
    let control = ListenerRuntime::new(
        metrics
            .register_listener("management", "http", "127.0.0.1:8081", None)
            .expect("control listener"),
    );
    let lease = control
        .admit_control(&generation)
        .expect("control admission");

    generation.stop_admitting_traffic();

    assert!(
        traffic
            .admit(&generation, crate::RuntimeReferenceKind::Http1)
            .is_err()
    );
    assert!(!generation.accepting());
    assert!(generation.control_accepting());
    assert!(generation.traffic_drained());
    assert!(!generation.drained());
    drop(
        control
            .admit_control(&generation)
            .expect("control admission after the drain"),
    );

    generation.stop_accepting();

    assert!(control.admit_control(&generation).is_err());
    drop(lease);
    assert!(generation.drained());
}
