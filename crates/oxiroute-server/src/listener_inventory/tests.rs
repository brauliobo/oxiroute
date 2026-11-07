use std::path::PathBuf;

use oxiroute_config::{ConfigDraft, ListenerBind, Management, Protocol, Stats, StatsPage};

use super::{
    ListenerDescriptorKind, ListenerInventory, ListenerMetricPolicy, ListenerRestartReason,
    descriptor_kind,
};

#[test]
fn inventory_core_is_platform_independent_and_unbounded() {
    let config = ConfigDraft {
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
    }
    .validate()
    .expect("valid empty inventory config");

    assert!(ListenerInventory::compile(&config).entries().is_empty());
    assert_eq!(
        descriptor_kind(
            Protocol::Http,
            &ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().expect("socket address"),
            },
        ),
        ListenerDescriptorKind::Tcp
    );
    assert_eq!(
        descriptor_kind(
            Protocol::Tcp,
            &ListenerBind::Unix {
                path: PathBuf::from("/tmp/inventory.sock"),
                mode: Some(0o600),
            },
        ),
        ListenerDescriptorKind::Unix
    );
    assert_eq!(
        descriptor_kind(
            Protocol::Udp,
            &ListenerBind::Udp {
                address: "127.0.0.1:8081".parse().expect("UDP address"),
            },
        ),
        ListenerDescriptorKind::Datagram
    );
    for protocol in [Protocol::Http3, Protocol::ForwardHttp3] {
        assert_eq!(
            descriptor_kind(
                protocol,
                &ListenerBind::Udp {
                    address: "127.0.0.1:8082".parse().expect("QUIC address"),
                },
            ),
            ListenerDescriptorKind::Quic
        );
    }
}

#[test]
fn descriptor_compatibility_ignores_runtime_policy_but_not_topology() {
    let active = inventory_config("edge", 8_080, Some(10));
    let mut policy_change = active.to_draft();
    policy_change.listeners[0].max_connections = Some(20);
    let policy_change = policy_change.validate().expect("valid policy change");
    let mut rename = active.to_draft();
    rename.listeners[0].name = "renamed".into();
    let rename = rename.validate().expect("valid rename");

    let active = ListenerInventory::compile(&active);
    assert!(active.same_descriptor_topology(&ListenerInventory::compile(&policy_change)));
    assert!(!active.same_descriptor_topology(&ListenerInventory::compile(&rename)));
    // Neither change needs a restart: a reload applies listener changes.
    assert!(!active.restart_required(&ListenerInventory::compile(&rename)));
    assert_eq!(active.complete_listener_count(), 1);
    assert_eq!(
        active.entries()[0].metric_policy,
        ListenerMetricPolicy::Public
    );
}

#[test]
fn only_a_unix_socket_that_keeps_its_path_and_changes_its_mode_needs_a_restart() {
    let active = matrix_config();
    let assert_restart = |candidate: oxiroute_config::ValidatedConfig, expected: bool| {
        let active_inventory = ListenerInventory::compile(&active);
        let candidate_inventory = ListenerInventory::compile(&candidate);
        assert_eq!(
            active_inventory.restart_required(&candidate_inventory),
            expected,
        );
        assert_eq!(
            active_inventory.restart_reason(&candidate_inventory),
            expected.then_some(ListenerRestartReason::UnixModeChange),
        );
    };
    let validated = |draft: ConfigDraft| draft.validate().expect("valid matrix candidate");

    assert_restart(active.clone(), false);

    let mut policy = active.to_draft();
    policy.listeners[0].max_connections = Some(99);
    policy.http_services[0].max_request_body_bytes = Some(32_768);
    assert_restart(validated(policy), false);

    let mut renamed = active.to_draft();
    renamed.listeners[0].name = "renamed-http".into();
    assert_restart(validated(renamed), false);

    let mut reordered = active.to_draft();
    reordered.listeners.swap(0, 1);
    assert_restart(validated(reordered), false);

    let mut protocol = active.to_draft();
    protocol.listeners[4].protocol = Protocol::Http;
    protocol.listeners[4].service = Some("web".into());
    assert_restart(validated(protocol), false);

    let mut bind = active.to_draft();
    bind.listeners[0].bind = ListenerBind::Socket {
        address: "127.0.0.1:8090".parse().unwrap(),
    };
    assert_restart(validated(bind), false);

    let mut unix_mode = active.to_draft();
    let ListenerBind::Unix { mode, .. } = &mut unix_mode.listeners[5].bind else {
        panic!("Unix fixture")
    };
    *mode = Some(0o660);
    assert_restart(validated(unix_mode), true);

    let mut removed_listener = active.to_draft();
    removed_listener.listeners.remove(0);
    assert_restart(validated(removed_listener), false);

    let mut no_management = active.to_draft();
    no_management.management = None;
    assert_restart(validated(no_management.clone()), false);
    let no_management = validated(no_management);
    assert!(
        !ListenerInventory::compile(&no_management)
            .restart_required(&ListenerInventory::compile(&active))
    );

    let mut stats_reordered = active.to_draft();
    stats_reordered.stats.as_mut().unwrap().binds.swap(0, 1);
    assert_restart(validated(stats_reordered), false);

    let mut no_stats = active.to_draft();
    no_stats.stats = None;
    assert_restart(validated(no_stats), false);

    let mut stats_page_added = active.to_draft();
    let second_page = StatsPage {
        bind: "127.0.0.1:8407".parse().unwrap(),
        uri_prefix: "/second".into(),
        refresh_ms: 2_000,
        admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    };
    stats_page_added
        .stats
        .as_mut()
        .unwrap()
        .pages
        .push(second_page);
    assert_restart(validated(stats_page_added.clone()), false);
    let two_pages = validated(stats_page_added);
    let mut pages_reordered = two_pages.to_draft();
    pages_reordered.stats.as_mut().unwrap().pages.swap(0, 1);
    assert!(
        !ListenerInventory::compile(&two_pages)
            .restart_required(&ListenerInventory::compile(&validated(pages_reordered)))
    );

    let mut udp = active.to_draft();
    udp.listeners[6].bind = ListenerBind::Udp {
        address: "127.0.0.1:8101".parse().unwrap(),
    };
    assert_restart(validated(udp), false);

    let mut h3 = active.to_draft();
    h3.listeners[7].protocol = Protocol::ForwardHttp3;
    h3.listeners[7].service = Some("forward".into());
    assert_restart(validated(h3), false);
}

#[test]
fn public_stats_page_display_name_rejects_traffic_collision_but_hidden_names_coexist() {
    let mut config = inventory_config("@stats-page-0", 8_080, None).to_draft();
    config.listeners.push(oxiroute_config::Listener {
        name: "@management".into(),
        bind: ListenerBind::Socket {
            address: ([127, 0, 0, 1], 8_081).into(),
        },
        protocol: Protocol::Tcp,
        service: Some("relay".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    config.management = Some(Management {
        bind: ([127, 0, 0, 1], 9_899).into(),
        ui_dir: None,
    });
    config.stats = Some(Stats {
        binds: vec![([127, 0, 0, 1], 9_900).into()],
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: ([127, 0, 0, 1], 9_901).into(),
            uri_prefix: "/stats".into(),
            refresh_ms: 1_000,
            admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
    });
    let config = config.validate().expect("valid typed collision config");
    let inventory = ListenerInventory::compile(&config);

    assert!(matches!(
        inventory.validate_public_display_names(),
        Err(crate::MetricsError::DuplicateListener(name)) if name == "@stats-page-0"
    ));
    assert_eq!(
        inventory
            .entries()
            .iter()
            .filter(|entry| entry.name == "@management")
            .count(),
        2
    );
}

fn inventory_config(
    name: &str,
    port: u16,
    max_connections: Option<u64>,
) -> oxiroute_config::ValidatedConfig {
    let mut config = ConfigDraft {
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
    };
    config.upstream_pools.push(oxiroute_config::UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![oxiroute_config::UpstreamEndpoint::Socket {
            address: "127.0.0.1:9000".parse().expect("upstream address"),
        }],
        algorithm: oxiroute_config::UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: oxiroute_config::HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        resilience: oxiroute_config::PoolResilience::default(),
    });
    config.l4_services.push(oxiroute_config::L4Service {
        name: "relay".into(),
        upstream_pool: "origin".into(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 1_000,
        lifetime_timeout_ms: None,
        proxy_protocol: None,
        udp: None,
    });
    config.listeners.push(oxiroute_config::Listener {
        name: name.into(),
        bind: ListenerBind::Socket {
            address: ([127, 0, 0, 1], port).into(),
        },
        protocol: Protocol::Tcp,
        service: Some("relay".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    config.validate().expect("valid inventory config")
}

#[allow(clippy::too_many_lines)]
fn matrix_config() -> oxiroute_config::ValidatedConfig {
    serde_json::from_value::<ConfigDraft>(serde_json::json!({
            "version": 1,
            "management": { "bind": "127.0.0.1:9900" },
            "stats": {
                "binds": ["127.0.0.1:8404", "127.0.0.1:8405"],
                "pages": [{
                    "bind": "127.0.0.1:8406",
                    "uri_prefix": "/stats",
                    "refresh_ms": 1000,
                    "admin": "disabled"
                }]
            },
            "certificates": [{
                "name": "downstream",
                "dns_names": ["proxy.example.test"],
                "source": {
                    "type": "files",
                    "certificate_chain_path": "/tmp/inventory-chain.pem",
                    "private_key_path": "/tmp/inventory-key.pem"
                }
            }],
            "tls_profiles": [{
                "name": "h3",
                "certificates": ["downstream"],
                "default_certificate": "downstream",
                "min_version": "1.3",
                "alpn": ["h3"]
            }],
            "listeners": [
                {"name":"http","bind":{"type":"socket","address":"127.0.0.1:7996"},"protocol":"http","service":"web"},
                {"name":"rtmp","bind":{"type":"socket","address":"127.0.0.1:7997"},"protocol":"rtmp","service":"live"},
                {"name":"forward-h1","bind":{"type":"socket","address":"127.0.0.1:7998"},"protocol":"forward_http1","service":"forward"},
                {"name":"forward-h2","bind":{"type":"socket","address":"127.0.0.1:7999"},"protocol":"forward_http2","service":"forward"},
                {"name":"tcp","bind":{"type":"socket","address":"127.0.0.1:8000"},"protocol":"tcp","service":"relay"},
                {"name":"unix","bind":{"type":"unix","path":"/tmp/inventory.sock","mode":384},"protocol":"tcp","service":"relay"},
                {"name":"udp","bind":{"type":"udp","address":"127.0.0.1:8001"},"protocol":"udp","service":"relay"},
                {"name":"h3","bind":{"type":"udp","address":"127.0.0.1:8002"},"protocol":"http3","service":"web","tls_profile":"h3"}
            ],
            "http_services": [{"name":"web","routes":[{
                "path":{"kind":"segment_prefix","value":"/"},
                "policy":{"request_buffering":true},
                "action":{"type":"fixed_response","status":200}
            }]}],
            "forward_proxy_services": [{"name":"forward","enabled_versions":["h1","h2","h3"],"tls_required":false}],
            "rtmp_services": [{"name":"live","applications":[{"name":"broadcast","live":true}]}],
            "upstream_pools": [{"name":"origin","endpoints":[{"type":"socket","address":"127.0.0.1:9000"}]}],
            "l4_services": [{"name":"relay","upstream_pool":"origin","udp":{}}]
        }))
        .expect("matrix draft")
        .validate()
        .expect("valid matrix config")
}
