#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;

use std::{fs, sync::Arc};

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, HttpAccessPolicy, HttpHostSelector,
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy,
    L4Service, Listener, ListenerBind, Protocol, RtmpApplication, RtmpService, SelfSignedKeyType,
    Stats, StatsPage, StatsPageAdminPolicy, TlsClientAuthMode, TlsClientAuthPolicy, TlsProfile,
    TlsVersion, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use oxiroute_rtmp::{RtmpCapabilities, RtmpRegistry};
use oxiroute_server::{
    RtmpManagementApi, RuntimeMetrics, RuntimePlan, TopologyEdgeKind, TopologyNode,
    TopologyNodeKind, runtime_plan,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use config_support::{
    empty_config, parsed_socket_bind as socket_bind, parsed_socket_endpoint as socket_endpoint,
};
use fixture_support::{create_secure_root, write_file_with_mode, write_test_identity};

#[test]
fn compiles_stable_redacted_nodes_and_typed_reference_edges() {
    let temp = TempDir::new().expect("TLS temp directory");
    let config = topology_config(&temp);

    let plan = runtime_plan(&config).expect("runtime plan");
    let mut reordered = config.clone();
    reordered.listeners.reverse();
    reordered.upstream_pools.reverse();
    reordered.upstream_pools[1].servers.reverse();
    let reordered_plan = runtime_plan(&reordered).expect("reordered runtime plan");
    let mut node_ids = plan
        .topology
        .nodes()
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let mut reordered_node_ids = reordered_plan
        .topology
        .nodes()
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    node_ids.sort_unstable();
    reordered_node_ids.sort_unstable();

    assert_eq!(node_ids, reordered_node_ids);
    for kind in [
        TopologyNodeKind::Listener,
        TopologyNodeKind::RtmpListener,
        TopologyNodeKind::TlsProfile,
        TopologyNodeKind::Certificate,
        TopologyNodeKind::HttpService,
        TopologyNodeKind::HttpRoute,
        TopologyNodeKind::L4Service,
        TopologyNodeKind::UpstreamPool,
        TopologyNodeKind::Endpoint,
    ] {
        assert!(plan.topology.nodes().iter().any(|node| node.kind == kind));
    }
    for kind in [
        TopologyEdgeKind::DispatchService,
        TopologyEdgeKind::ServiceRoute,
        TopologyEdgeKind::RoutePool,
        TopologyEdgeKind::ServicePool,
        TopologyEdgeKind::PoolEndpoint,
        TopologyEdgeKind::ListenerTls,
        TopologyEdgeKind::TlsCertificate,
    ] {
        assert!(plan.topology.edges().iter().any(|edge| edge.kind == kind));
    }

    let route = plan
        .topology
        .nodes()
        .iter()
        .find(|node| node.kind == TopologyNodeKind::HttpRoute)
        .expect("HTTP route node");
    assert_eq!(route.config_path, "/http_services/0/routes/0");
    assert_eq!(
        route.attributes["host"],
        serde_json::json!({
            "kind": "normalized_host",
            "value": "api.example.test",
        })
    );
    assert_eq!(
        route.attributes["path"],
        serde_json::json!({
            "kind": "segment_prefix",
            "value": "/v1",
        })
    );
    assert_eq!(route.attributes["action"]["type"], "proxy");
    assert_eq!(route.attributes["action"]["upstreamPool"], "api");

    assert_canonical_listener_service_and_endpoint_attributes(&plan);
    assert_private_key_redaction(&plan, &config);
    let tls_profile = node(&plan, TopologyNodeKind::TlsProfile, "public");
    assert_eq!(
        tls_profile.attributes["clientAuth"],
        json!({
            "mode": "required",
            "caConfigured": true,
            "allowedDnsNameCount": 1,
        })
    );
    let serialized = serde_json::to_string(tls_profile).expect("TLS profile topology JSON");
    assert!(!serialized.contains("client-ca.pem"));
}

#[test]
fn topology_marks_self_signed_certificates_as_development_only() {
    let temp = TempDir::new().expect("TLS temp directory");
    let mut config = topology_config(&temp);
    config.certificates[0].source = CertificateSource::SelfSignedDevelopment {
        validity_days: 7,
        key_type: SelfSignedKeyType::EcdsaP256,
    };

    let plan = runtime_plan(&config).expect("self-signed runtime plan");
    let certificate = plan
        .topology
        .nodes()
        .iter()
        .find(|node| node.kind == TopologyNodeKind::Certificate)
        .expect("certificate node");
    assert_eq!(
        certificate.attributes["source"],
        json!({
            "type": "self_signed_development",
            "developmentOnly": true,
            "validityDays": 7,
            "keyType": "ecdsa_p256",
        })
    );
    let serialized = serde_json::to_string(certificate).expect("certificate JSON");
    assert!(!serialized.contains("PRIVATE KEY"));
}

fn assert_canonical_listener_service_and_endpoint_attributes(plan: &RuntimePlan) {
    let web = node(plan, TopologyNodeKind::Listener, "web");
    assert_eq!(
        web.attributes["bind"],
        serde_json::json!({
            "type": "socket",
            "address": "127.0.0.1:8443",
        })
    );
    assert!(web.attributes["maxConnections"].is_null());
    let database = node(plan, TopologyNodeKind::Listener, "database");
    assert_eq!(
        database.attributes["bind"],
        serde_json::json!({
            "type": "unix",
            "path": "/run/oxiroute/database.sock",
            "mode": null,
        })
    );

    let service = node(plan, TopologyNodeKind::HttpService, "api");
    assert!(service.attributes["maxRequestBodyBytes"].is_null());
    let pool = node(plan, TopologyNodeKind::UpstreamPool, "api");
    assert_eq!(pool.attributes["algorithm"], "least_connections");

    let mut endpoint_ids = plan
        .topology
        .nodes()
        .iter()
        .filter(|node| node.kind == TopologyNodeKind::Endpoint)
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    endpoint_ids.sort_unstable();
    assert_eq!(
        endpoint_ids,
        [
            "upstream_server:3:api:5:api-1",
            "upstream_server:3:api:5:api-2",
            "upstream_server:3:api:5:api-3",
            "upstream_server:8:database:10:database-1",
        ]
    );
    let endpoints = plan
        .topology
        .nodes()
        .iter()
        .filter(|node| node.kind == TopologyNodeKind::Endpoint)
        .map(|node| (node.id.as_str(), &node.attributes))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        endpoints["upstream_server:3:api:5:api-1"],
        &serde_json::json!({
            "type": "socket",
            "address": "127.0.0.1:3000",
            "maxConnections": null,
            "serverName": "api-1",
        })
    );
    assert_eq!(
        endpoints["upstream_server:3:api:5:api-2"],
        &serde_json::json!({
            "type": "dns",
            "host": "backend.example.test",
            "port": 3001,
            "address": "backend.example.test:3001",
            "maxConnections": null,
            "serverName": "api-2",
        })
    );
    assert_eq!(
        endpoints["upstream_server:3:api:5:api-3"],
        &serde_json::json!({
            "type": "unix",
            "path": "/run/oxiroute/api.sock",
            "address": "/run/oxiroute/api.sock",
            "maxConnections": null,
            "serverName": "api-3",
        })
    );
}

fn assert_private_key_redaction(plan: &RuntimePlan, config: &Config) {
    let certificate = plan
        .topology
        .nodes()
        .iter()
        .find(|node| node.kind == TopologyNodeKind::Certificate)
        .expect("certificate node");
    assert_eq!(
        certificate.attributes["source"]["privateKeyPath"],
        "<redacted>"
    );
    assert_ne!(
        certificate.attributes["source"]["certificateChainPath"],
        "<redacted>"
    );
    let private_key_path = match &config.certificates[0].source {
        CertificateSource::Files {
            private_key_path, ..
        } => private_key_path,
        CertificateSource::Certbot { .. } | CertificateSource::SelfSignedDevelopment { .. } => {
            unreachable!("test uses file identity")
        }
        CertificateSource::AcmeManaged { .. } => unreachable!("test uses file identity"),
    };
    let serialized = serde_json::to_string(certificate).expect("certificate JSON");
    assert!(!serialized.contains(private_key_path.to_str().expect("UTF-8 key path")));
}

#[test]
fn serves_active_topology_with_name_joined_runtime_overlays() {
    let temp = TempDir::new().expect("TLS temp directory");
    let config = topology_config(&temp);
    let plan = runtime_plan(&config).expect("runtime plan");
    let metrics = RuntimeMetrics::new();
    metrics
        .register_upstream_pools(plan.pools.iter().cloned())
        .expect("pool metrics");
    for (listener, service) in config.listeners.iter().zip(&plan.services) {
        let listener_metrics = metrics
            .register_configured_listener(
                &listener.name,
                service.kind.protocol(),
                &listener.bind,
                listener.max_connections,
            )
            .expect("listener metrics");
        listener_metrics.mark_listening();
    }
    let web = metrics
        .listener("web")
        .expect("listener registry")
        .expect("web listener");
    let _connection = web.begin_connection().expect("active connection");
    let endpoint_lease = plan.pools[0].select().expect("API endpoint lease");
    assert_eq!(endpoint_lease.endpoint().to_string(), "127.0.0.1:3000");
    let api = RtmpManagementApi::new(
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        })),
        metrics,
        Arc::clone(&plan.topology),
    );

    let response = api.handle("GET", "/api/v1/topology", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("topology JSON");

    assert_eq!(response.status, 200);
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["state"]["config"], "active");
    assert_eq!(body["state"]["runtime"], "active");
    assert!(body["state"]["sampledAtUnixMs"].as_u64().is_some());
    assert_eq!(body["nodes"].as_array().map(Vec::len), Some(14));
    assert_eq!(body["edges"].as_array().map(Vec::len), Some(11));

    let overlays = body["overlays"].as_array().expect("runtime overlays");
    assert_eq!(overlays.len(), 9);
    let web_overlay = overlays
        .iter()
        .find(|overlay| overlay["nodeId"] == "listener:3:web")
        .expect("web runtime overlay");
    assert_eq!(web_overlay["state"], "listening");
    assert_eq!(web_overlay["metrics"]["activeConnections"], 1);
    assert_eq!(web_overlay["metrics"]["acceptedConnections"], "1");
    assert_eq!(web_overlay["metrics"]["rejectedConnections"], "0");
    assert_eq!(web_overlay["metrics"]["bytesReceived"], "0");
    assert_eq!(web_overlay["metrics"]["bytesSent"], "0");
    let api_pool = overlays
        .iter()
        .find(|overlay| overlay["nodeId"] == "upstream_pool:3:api")
        .expect("API pool overlay");
    assert_eq!(api_pool["state"], "available");
    assert_eq!(api_pool["metrics"]["availableEndpoints"], 3);
    for (endpoint_id, active_connections) in [
        ("upstream_server:3:api:5:api-1", "1"),
        ("upstream_server:3:api:5:api-2", "0"),
        ("upstream_server:3:api:5:api-3", "0"),
        ("upstream_server:8:database:10:database-1", "0"),
    ] {
        let endpoint = overlays
            .iter()
            .find(|overlay| overlay["nodeId"] == endpoint_id)
            .expect("endpoint overlay joined by canonical identity");
        assert_eq!(endpoint["state"], "unchecked");
        assert_eq!(endpoint["metrics"]["activeConnections"], active_connections);
    }

    let body_text = String::from_utf8(response.body).expect("UTF-8 topology body");
    let private_key_path = match &config.certificates[0].source {
        CertificateSource::Files {
            private_key_path, ..
        } => private_key_path,
        CertificateSource::Certbot { .. } | CertificateSource::SelfSignedDevelopment { .. } => {
            unreachable!("test uses file identity")
        }
        CertificateSource::AcmeManaged { .. } => unreachable!("test uses file identity"),
    };
    assert!(!body_text.contains(private_key_path.to_str().expect("UTF-8 key path")));
    assert!(!body_text.contains("client-ca.pem"));
    let status = api.handle("GET", "/api/v1/status", 100);
    let status_body: Value = serde_json::from_slice(&status.body).expect("status JSON");
    assert_eq!(status.status, 200);
    assert_eq!(
        status_body["tlsProfiles"][0]["clientAuth"],
        json!({
            "mode": "required",
            "caConfigured": true,
            "allowedDnsNameCount": 1,
        })
    );
    assert!(!status.body.windows("client-ca.pem".len()).any(|window| window == b"client-ca.pem"));
    assert_eq!(api.handle("POST", "/api/v1/topology", 100).status, 405);

    web.mark_failed();
    let degraded = api.handle("GET", "/api/v1/topology", 101);
    let degraded_body: Value =
        serde_json::from_slice(&degraded.body).expect("degraded topology JSON");
    assert_eq!(degraded.status, 200);
    assert_eq!(degraded_body["state"]["runtime"], "degraded");
    let web_overlay = degraded_body["overlays"]
        .as_array()
        .and_then(|overlays| {
            overlays
                .iter()
                .find(|overlay| overlay["nodeId"] == "listener:3:web")
        })
        .expect("failed web runtime overlay");
    assert_eq!(web_overlay["state"], "failed");
}

#[test]
fn serves_topology_with_a_stats_page_runtime_overlay() {
    let temp = TempDir::new().expect("TLS temp directory");
    let mut config = topology_config(&temp);
    let page_bind = "127.0.0.1:18405".parse().expect("stats page bind");
    config.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: page_bind,
            uri_prefix: "/stats".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Disabled,
            max_connections: Some(20),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
    });
    let plan = runtime_plan(&config).expect("runtime plan");
    let metrics = RuntimeMetrics::new();
    metrics
        .register_upstream_pools(plan.pools.iter().cloned())
        .expect("pool metrics");
    for (listener, service) in config.listeners.iter().zip(&plan.services) {
        metrics
            .register_configured_listener(
                &listener.name,
                service.kind.protocol(),
                &listener.bind,
                listener.max_connections,
            )
            .expect("listener metrics");
    }
    metrics
        .register_configured_listener(
            "@stats-page-0",
            "http",
            &ListenerBind::Socket { address: page_bind },
            Some(20),
        )
        .expect("stats page metrics")
        .mark_listening();
    let api = RtmpManagementApi::new(
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        })),
        metrics,
        Arc::clone(&plan.topology),
    );

    let response = api.handle("GET", "/api/v1/topology", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("topology JSON");

    assert_eq!(response.status, 200);
    let node = body["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["name"] == "@stats-page-0"))
        .expect("stats page node");
    assert_eq!(node["configPath"], "/stats/pages/0");
    assert_eq!(
        node["attributes"]["bind"],
        json!({ "type": "socket", "address": page_bind })
    );
    assert!(body["overlays"].as_array().is_some_and(|overlays| {
        overlays
            .iter()
            .any(|overlay| overlay["nodeId"] == node["id"])
    }));
}

#[test]
fn action_aware_topology_never_serializes_access_tokens_or_filesystem_roots() {
    let temp = TempDir::new().expect("topology action directory");
    let root = create_secure_root(temp.path(), "private-static-root");
    fs::write(root.join("index.html"), b"private").expect("write private static file");
    let token = "topology-token-0123456789abcdef0123456789abcdef";
    let token_path =
        write_file_with_mode(temp.path(), "private-route-token", token.as_bytes(), 0o600);
    let mut config = topology_config(&temp);
    config.http_services[0].routes.push(HttpRoute {
        host: None,
        path: HttpPathSelector::Exact {
            value: "/private".into(),
        },
        methods: vec!["GET".into()],
        access_policy: Some(HttpAccessPolicy::BearerTokenFile {
            token_file_path: token_path.clone(),
            header_name: "authorization".into(),
            realm: Some("private".into()),
        }),
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::StaticFiles {
            root_directory: root.clone(),
            path_mapping: oxiroute_config::HttpStaticPathMapping::default(),
            index_files: vec!["index.html".into()],
            internal_index_redirects: false,
            directory_redirects: false,
            spa_fallback: None,
            try_files: Vec::new(),
            autoindex: false,
            autoindex_exact_size: true,
            autoindex_local_time: false,
            etag: true,
            mime: oxiroute_config::HttpStaticMimePolicy::default(),
            headers: Vec::new(),
            error_responses: Vec::new(),
        },
    });

    let plan = runtime_plan(&config).expect("action-aware topology plan");
    let route = plan
        .topology
        .nodes()
        .iter()
        .find(|node| {
            node.kind == TopologyNodeKind::HttpRoute
                && node.attributes["action"]["type"] == "static_files"
        })
        .expect("static route topology node");
    assert_eq!(route.attributes["access"]["type"], "bearer_token_file");
    assert_eq!(route.attributes["action"]["spaFallback"], false);
    let serialized = serde_json::to_string(plan.topology.nodes()).expect("topology nodes JSON");
    assert!(!serialized.contains(token));
    assert!(!serialized.contains(&token_path.display().to_string()));
    assert!(!serialized.contains(&root.display().to_string()));
}

#[allow(clippy::too_many_lines)]
fn topology_config(temp: &TempDir) -> Config {
    let (certificate_chain_path, private_key_path) =
        write_test_identity(temp.path(), "topology-private-key-do-not-expose.pem");
    let mut config = Config {
        certificates: vec![Certificate {
            name: "public".into(),
            dns_names: vec!["proxy.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path,
                private_key_path,
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "public".into(),
            certificates: vec!["public".into()],
            default_certificate: "public".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            policy: oxiroute_config::TlsPolicy::default(),
        }],
        listeners: topology_listeners(),
        upstream_pools: vec![
            UpstreamPool {
                name: "api".into(),
                servers: vec![
                    upstream_server("api-1", socket_endpoint("[::ffff:127.0.0.1]:3000")),
                    upstream_server(
                        "api-2",
                        UpstreamEndpoint::Dns {
                            host: "BACKEND.EXAMPLE.TEST".into(),
                            port: 3001,
                        },
                    ),
                    upstream_server(
                        "api-3",
                        UpstreamEndpoint::Unix {
                            path: "/run//oxiroute///api.sock".into(),
                        },
                    ),
                ],
                endpoints: Vec::new(),
                algorithm: UpstreamAlgorithm::LeastConnections,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: oxiroute_config::UpstreamConnectionReuse::Never,
            },
            UpstreamPool {
                name: "database".into(),
                servers: vec![upstream_server(
                    "database-1",
                    socket_endpoint("127.0.0.1:5432"),
                )],
                endpoints: Vec::new(),
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
            },
        ],
        http_services: vec![HttpService {
            name: "api".into(),
            routes: vec![HttpRoute {
                host: Some(HttpHostSelector::NormalizedHost {
                    value: "api.example.test".into(),
                }),
                path: HttpPathSelector::SegmentPrefix {
                    value: "/v1".into(),
                },
                methods: vec!["GET".into()],
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "api".into(),
                    policy: HttpProxyPolicy {
                        retry: oxiroute_config::HttpRetryPolicy {
                            max_retries: 1,
                            ..oxiroute_config::HttpRetryPolicy::default()
                        },
                        ..HttpProxyPolicy::default()
                    },
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 15_000,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
            outbound_chunk_size: 4_096,
            access_log: None,
            outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            exec_profiles: Vec::new(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: true,
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
        }],
        l4_services: vec![L4Service {
            name: "database".into(),
            upstream_pool: "database".into(),
            connect_timeout_ms: 5_000,
            idle_timeout_ms: 120_000,
            lifetime_timeout_ms: Some(600_000),
            proxy_protocol: None,
            udp: None,
        }],
        ..empty_config()
    };
    config.tls_profiles[0].policy.client_auth = TlsClientAuthPolicy {
        mode: TlsClientAuthMode::Required,
        ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        allowed_dns_names: vec!["client.example.test".into()],
    };
    config
}

fn upstream_server(name: &str, endpoint: UpstreamEndpoint) -> UpstreamServer {
    UpstreamServer {
        name: name.into(),
        endpoint,
        max_connections: None,
        dns_resolution: oxiroute_config::DnsResolutionPolicy::default(),
    }
}

fn topology_listeners() -> Vec<Listener> {
    vec![
        Listener {
            name: "web".into(),
            bind: socket_bind("[::ffff:127.0.0.1]:8443"),
            protocol: Protocol::Http,
            service: Some("api".into()),
            tls_profile: Some("public".into()),
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        },
        Listener {
            name: "database".into(),
            bind: ListenerBind::Unix {
                path: "/run//oxiroute///database.sock".into(),
                mode: None,
            },
            protocol: Protocol::Tcp,
            service: Some("database".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(100),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        },
        Listener {
            name: "live".into(),
            bind: socket_bind("127.0.0.1:1935"),
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(50),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        },
    ]
}

fn node<'a>(plan: &'a RuntimePlan, kind: TopologyNodeKind, name: &str) -> &'a TopologyNode {
    plan.topology
        .nodes()
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .expect("topology node")
}
