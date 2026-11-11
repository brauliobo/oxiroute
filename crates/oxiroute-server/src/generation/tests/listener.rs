#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn public_stats_page_name_collision_fails_before_process_registration() {
    let mut config = colliding_listener_document().validated_config.to_draft();
    config.listeners[0].name = "@stats-page-0".into();
    config
        .stats
        .as_mut()
        .expect("stats")
        .pages
        .push(oxiroute_config::StatsPage {
            bind: unused_tcp_address(),
            uri_prefix: "/stats".into(),
            refresh_ms: 1_000,
            admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
    let process = ProcessRuntime::new(None);

    assert!(matches!(
        PreparedGeneration::prepare_from(
            document_for(&config),
            ListenerSource::BindOrReuse {
                previous: None,
                process: process.clone(),
            },
        ),
        Err(GenerationError::Metrics(MetricsError::DuplicateListener(name)))
            if name == "@stats-page-0"
    ));
    assert_eq!(process.listener_count(), 0);
}

#[test]
fn prepared_generation_uses_typed_listener_identity_for_colliding_display_names() {
    let document = colliding_listener_document();
    let config = Arc::new(document.validated_config);
    let inventory = ListenerInventory::compile(&config);
    let plan = runtime_plan(&config).expect("collision runtime plan");
    let acquired = acquire_runtime_services(&plan).expect("collision runtime acquisition");
    let rtmp =
        PreparedGeneration::prepare_rtmp(acquired.services()).expect("collision RTMP preparation");
    let reservations =
        ListenerReservations::prepare(&config, None).expect("collision listener reservations");
    let process = ProcessRuntime::new(None);
    let metrics = RuntimeMetrics::for_process(process.clone());
    let registration = metrics
        .register_inventory(inventory.entries())
        .expect("collision inventory registration");
    let prepared = PreparedGeneration {
        config,
        inventory,
        metrics,
        resources: GenerationResources::commit(plan, acquired, reservations, registration, rtmp),
        revision: GenerationRevision {
            disk: document.authored_revision,
            candidate: document.effective_revision,
        },
    };
    let generation = RuntimeGeneration::activate(prepared);

    assert_eq!(
        generation
            .metrics
            .internal_listener_snapshots()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        generation
            .traffic_listener_metrics("@management")
            .unwrap()
            .name(),
        "@management"
    );
    assert_eq!(
        generation.management_listener_metrics().unwrap().name(),
        "@management"
    );
    assert_eq!(
        generation
            .traffic_listener_metrics("@stats-0")
            .unwrap()
            .name(),
        "@stats-0"
    );
    assert_eq!(
        generation.stats_listener_metrics(0).unwrap().name(),
        "@stats-0"
    );
    assert_eq!(generation.metrics.snapshot().unwrap().listeners.len(), 2);
    assert_eq!(process.listener_count(), 4);
}

#[test]
fn bounded_drain_waits_for_all_protocol_reference_kinds() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("prepare");
    let generation = manager.activate(&candidate).expect("activate");
    let references = [
        RuntimeReferenceKind::ForwardHttp1,
        RuntimeReferenceKind::ForwardHttp3,
        RuntimeReferenceKind::Http1,
        RuntimeReferenceKind::Http2,
        RuntimeReferenceKind::WebSocket,
        RuntimeReferenceKind::Tcp,
        RuntimeReferenceKind::Rtmp,
        RuntimeReferenceKind::Udp,
        RuntimeReferenceKind::Http3,
    ]
    .map(|kind| generation.begin_reference(kind).expect("reference"));
    let draining = Arc::clone(&generation);
    let waiter = thread::spawn(move || draining.drain(Duration::from_secs(1)));
    thread::sleep(Duration::from_millis(20));
    assert!(!generation.accepting());
    drop(references);
    assert!(waiter.join().expect("drain thread"));
}

#[test]
fn admission_claim_and_protocol_reference_have_independent_lifetimes() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("prepare");
    let generation = manager.activate(&candidate).expect("activate");
    let admission = generation.begin_admission().expect("admission claim");

    assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 0);

    let reference = generation
        .begin_reference(RuntimeReferenceKind::Http1)
        .expect("HTTP/1 reference");
    drop(admission);

    assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 1);
    assert!(!generation.drain(Duration::ZERO));

    drop(reference);
    assert!(generation.drain(Duration::from_millis(100)));
}

#[test]
fn owned_reference_remains_after_acceptance_closes_for_handoff() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("prepare");
    let generation = manager.activate(&candidate).expect("activate");
    let reference = generation.begin_owned_reference(RuntimeReferenceKind::Http1);

    generation.stop_accepting();

    assert!(!generation.accepting());
    assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 1);
    drop(reference);
    assert!(generation.drain(Duration::from_millis(100)));
}

#[test]
fn listener_runtime_rolls_back_generation_ownership_when_metrics_reject() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("prepare");
    let generation = manager.activate(&candidate).expect("activate");
    let metrics = RuntimeMetrics::with_max_connections(Some(1));
    let listener = metrics
        .register_listener("edge", "http", "127.0.0.1:8080", Some(1))
        .expect("listener");
    let held = listener.begin_connection().expect("held connection");
    let runtime = crate::listener::runtime::ListenerRuntime::new(listener);

    assert!(
        runtime
            .admit(&generation, RuntimeReferenceKind::Http1)
            .is_err()
    );
    assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 0);
    drop(held);
    assert!(generation.drain(Duration::from_millis(100)));
}

#[test]
fn listener_runtime_owned_admission_does_not_recheck_a_closed_generation_gate() {
    let manager = GenerationManager::new();
    let candidate = manager.prepare(document()).expect("prepare");
    let generation = manager.activate(&candidate).expect("activate");
    let metrics = RuntimeMetrics::new();
    let listener = metrics
        .register_listener("edge", "http", "127.0.0.1:8080", Some(1))
        .expect("listener");
    let runtime = crate::listener::runtime::ListenerRuntime::new(listener);

    generation.stop_accepting();
    let lease = runtime
        .admit_owned(&generation, RuntimeReferenceKind::Http1)
        .expect("owned admission after gate close");

    assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 1);
    drop(lease);
    assert!(generation.drain(Duration::from_millis(100)));
}

#[test]
fn failed_listener_candidate_releases_resources_and_preserves_active_generation() {
    let manager = GenerationManager::new();
    let active_candidate = manager.prepare(document()).expect("prepare active");
    let active = manager.activate(&active_candidate).expect("activate");
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied");
    let address = occupied.local_addr().expect("address");
    let mut candidate = active.config().to_draft();
    candidate.listeners.push(oxiroute_config::Listener {
        name: "live".into(),
        bind: oxiroute_config::ListenerBind::Socket { address },
        protocol: oxiroute_config::Protocol::Rtmp,
        service: Some("live".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    candidate.rtmp_services.push(oxiroute_config::RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        max_inbound_message_size: 8 * 1024 * 1024,
        ack_window_size: 5_000_000,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![oxiroute_config::RtmpApplication {
            name: "live".into(),
            live: true,
            idle_streams: false,
            publish: oxiroute_config::RtmpAccessPolicy::default(),
            play: oxiroute_config::RtmpAccessPolicy::default(),
            limits: oxiroute_config::RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: Vec::new(),
        }],
    });

    assert!(manager.prepare(document_for(&candidate)).is_err());
    assert!(Arc::ptr_eq(
        &manager.active().expect("still active"),
        &active
    ));
    assert!(active.accepting());
    assert!(manager.candidate().is_none());
}
