use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use super::*;

fn empty_topology() -> TopologySnapshot {
    TopologySnapshot::compile(&oxiroute_config::ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
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
    let active = serde_json::to_value(
        TopologyResponse::active(&topology, &runtime).expect("active topology projection"),
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
    assert_eq!(
        active,
        topology
            .response_value(&runtime)
            .expect("legacy active topology JSON")
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
