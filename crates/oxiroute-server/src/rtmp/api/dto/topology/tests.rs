use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use super::*;
use crate::{EndpointHealthSnapshot, EndpointHealthState, PoolHealthSnapshot};

fn empty_topology() -> TopologySnapshot {
    TopologySnapshot::compile(&oxiroute_config::ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
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
        log_filter: None,
    })
}

fn schema() -> Value {
    let generator = SchemaSettings::draft2020_12().into_generator();
    serde_json::to_value(generator.into_root_schema_for::<TopologyResponse>())
        .expect("topology response schema")
}

#[test]
fn active_and_candidate_projections_preserve_the_v1_golden_objects() {
    let topology = empty_topology();
    let runtime = RuntimeHealthSnapshot {
        sampled_at_unix_ms: 42,
        listeners: Vec::new(),
        upstream_pools: Vec::new(),
    };
    let active: Value = serde_json::from_slice(
        &TopologyResponse::active_body(&topology, &runtime).expect("active topology body"),
    )
    .expect("active topology JSON");
    let candidate = serde_json::to_value(
        TopologyResponse::candidate(&topology, 43).expect("candidate topology projection"),
    )
    .expect("candidate topology JSON");

    assert_eq!(
        active,
        json!({
            "schemaVersion": 1,
            "state": {
                "config": "active",
                "runtime": "active",
                "sampledAtUnixMs": 42,
            },
            "nodes": [],
            "edges": [],
            "overlays": [],
        })
    );
    assert_eq!(
        candidate,
        json!({
            "schemaVersion": 1,
            "state": {
                "config": "candidate",
                "runtime": "not_active",
                "sampledAtUnixMs": 43,
            },
            "nodes": [],
            "edges": [],
            "overlays": [],
        })
    );
}

#[test]
fn topology_schema_is_structural_and_discriminates_nodes_and_overlays() {
    let schema = schema();
    let encoded = schema.to_string();

    assert!(!encoded.contains("serde_json::Value"));
    assert!(!encoded.contains("\"additionalProperties\":true"));
    for kind in [
        "listener",
        "forward_proxy_listener",
        "forward_proxy_service",
        "rtmp_listener",
        "tls_profile",
        "certificate",
        "http_service",
        "http_route",
        "l4_service",
        "upstream_pool",
        "endpoint",
    ] {
        assert!(encoded.contains(&format!("\"const\":\"{kind}\"")));
    }
    for metrics in [
        "ListenerOverlayMetricsDto",
        "PoolOverlayMetricsDto",
        "EndpointOverlayMetricsDto",
    ] {
        assert!(encoded.contains(metrics), "schema omitted {metrics}");
    }
    assert_eq!(schema["$defs"]["DecimalCounter"]["type"], "string");
}

#[test]
fn topology_node_projection_is_exhaustive_over_domain_kinds() {
    let names = [
        TopologyNodeKind::Listener,
        TopologyNodeKind::ForwardProxyListener,
        TopologyNodeKind::ForwardProxyService,
        TopologyNodeKind::RtmpListener,
        TopologyNodeKind::TlsProfile,
        TopologyNodeKind::Certificate,
        TopologyNodeKind::HttpService,
        TopologyNodeKind::HttpRoute,
        TopologyNodeKind::L4Service,
        TopologyNodeKind::UpstreamPool,
        TopologyNodeKind::Endpoint,
    ]
    .map(node_kind_name);

    assert_eq!(
        names,
        [
            "listener",
            "forward_proxy_listener",
            "forward_proxy_service",
            "rtmp_listener",
            "tls_profile",
            "certificate",
            "http_service",
            "http_route",
            "l4_service",
            "upstream_pool",
            "endpoint",
        ]
    );
}

#[test]
fn topology_schema_excludes_secret_bearing_source_fields() {
    let schema = schema().to_string().to_ascii_lowercase();
    for forbidden in [
        "tokenfilepath",
        "htpasswdfilepath",
        "rootdirectory",
        "contacts",
        "accountkey",
        "credentials",
    ] {
        assert!(!schema.contains(forbidden), "schema exposed {forbidden}");
    }
}

fn pool_topology() -> TopologySnapshot {
    let config = serde_json::from_value::<oxiroute_config::ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": [{
            "name": "api",
            "servers": [
                { "name": "a", "endpoint": { "type": "socket", "address": "127.0.0.1:9001" } },
                { "name": "b", "endpoint": { "type": "socket", "address": "127.0.0.1:9002" } },
            ],
        }],
    }))
    .expect("pool configuration");
    TopologySnapshot::compile(&config)
}

fn endpoint_snapshot(
    name: &str,
    origin: crate::ServerOrigin,
    active: u64,
) -> EndpointHealthSnapshot {
    EndpointHealthSnapshot {
        active_connections: active,
        administrative_state: crate::AdministrativeState::Ready,
        address: crate::RuntimeEndpoint::Socket {
            address: "127.0.0.1:9003".parse().expect("address"),
        },
        checks_enabled: false,
        checks_running: false,
        configured_max_connections: None,
        health_override: crate::HealthOverride::Auto,
        max_connections: Some(7),
        name: name.into(),
        state: EndpointHealthState::Healthy,
        configured_weight: 1,
        weight: 1,
        origin,
        last_checked_at_unix_ms: None,
        last_transition_at_unix_ms: Some(5),
        successful_checks: 3,
        failed_checks: 1,
        consecutive_successes: 2,
        consecutive_failures: 0,
        last_failure: Some(crate::HealthFailure::Slow),
        passive_ejected: false,
        passive_failure_count: 0,
        passive_consecutive_failures: 0,
        passive_ejection_count: 0,
        passive_ejection_reason: None,
        passive_ejected_at_unix_ms: None,
        passive_ejection_until_unix_ms: None,
        passive_recovery_count: 0,
        passive_last_recovery_at_unix_ms: None,
    }
}

fn pool_snapshot(endpoints: Vec<EndpointHealthSnapshot>) -> PoolHealthSnapshot {
    PoolHealthSnapshot {
        name: "api".into(),
        algorithm: "round_robin",
        available_endpoints: endpoints.len(),
        total_endpoints: endpoints.len(),
        unavailable_selections: 4,
        fail_open_selections: 0,
        queued: 1,
        queued_by_class: Vec::new(),
        oldest_queue_wait_ms: 0,
        queued_total: 9,
        queue_timeouts: 2,
        queue_cancellations: 1,
        canary: None,
        endpoints,
    }
}

#[test]
fn active_body_is_the_compiled_graph_plus_live_overlays_and_registered_servers() {
    let topology = pool_topology();
    let runtime = |active| RuntimeHealthSnapshot {
        sampled_at_unix_ms: 42,
        listeners: Vec::new(),
        upstream_pools: vec![pool_snapshot(vec![
            endpoint_snapshot("a", crate::ServerOrigin::Static, active),
            endpoint_snapshot("b", crate::ServerOrigin::Static, 0),
            endpoint_snapshot("c", crate::ServerOrigin::Dynamic, 0),
        ])],
    };
    let first: Value =
        serde_json::from_slice(&TopologyResponse::active_body(&topology, &runtime(1)).unwrap())
            .unwrap();
    let second: Value =
        serde_json::from_slice(&TopologyResponse::active_body(&topology, &runtime(2)).unwrap())
            .unwrap();

    let nodes = first["nodes"].as_array().unwrap();
    let compiled = topology.nodes().len();
    assert_eq!(nodes.len(), compiled + 1);
    assert_eq!(
        Value::Array(nodes[..compiled].to_vec()),
        serde_json::to_value(topology.nodes()).unwrap()
    );
    let registered = &nodes[compiled];
    assert_eq!(registered["name"], "c");
    assert_eq!(registered["attributes"]["origin"], "dynamic");
    assert_eq!(registered["attributes"]["address"], "127.0.0.1:9003");
    assert_eq!(registered["attributes"]["maxConnections"], 7);
    assert_eq!(
        first["edges"].as_array().unwrap().len(),
        topology.edges().len() + 1
    );
    let overlays = first["overlays"].as_array().unwrap();
    assert_eq!(overlays.len(), 4);
    assert_eq!(overlays[0]["state"], "available");
    assert_eq!(overlays[0]["metrics"]["queuedTotal"], "9");
    assert_eq!(overlays[1]["metrics"]["activeConnections"], "1");
    assert_eq!(overlays[1]["metrics"]["lastFailure"], "slow");
    assert_eq!(overlays[3]["nodeId"], registered["id"]);
    // Only the overlays move between two calls on the same generation; the compiled part is the cached bytes.
    assert_eq!(second["nodes"], first["nodes"]);
    assert_eq!(second["edges"], first["edges"]);
    assert_eq!(second["overlays"][1]["metrics"]["activeConnections"], "2");
    assert!(serde_json::from_value::<TopologyResponse>(first).is_ok());
}

#[test]
fn active_body_names_what_the_runtime_lacks_or_does_not_know() {
    let topology = pool_topology();
    let body = |endpoints: Vec<EndpointHealthSnapshot>| {
        TopologyResponse::active_body(
            &topology,
            &RuntimeHealthSnapshot {
                sampled_at_unix_ms: 1,
                listeners: Vec::new(),
                upstream_pools: vec![pool_snapshot(endpoints)],
            },
        )
    };
    let missing = body(vec![endpoint_snapshot("a", crate::ServerOrigin::Static, 0)]).unwrap_err();
    assert!(
        missing.contains("compiled server `b` in pool `api`"),
        "{missing}"
    );
    let unknown = body(vec![
        endpoint_snapshot("a", crate::ServerOrigin::Static, 0),
        endpoint_snapshot("b", crate::ServerOrigin::Static, 0),
        endpoint_snapshot("z", crate::ServerOrigin::Static, 0),
    ])
    .unwrap_err();
    assert!(
        unknown.contains("active server `z` in pool `api`"),
        "{unknown}"
    );
    let no_pool = TopologyResponse::active_body(
        &topology,
        &RuntimeHealthSnapshot {
            sampled_at_unix_ms: 1,
            listeners: Vec::new(),
            upstream_pools: Vec::new(),
        },
    )
    .unwrap_err();
    assert!(
        no_pool.contains("compiled upstream pool `api`"),
        "{no_pool}"
    );
}
