#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use http::{Method, Uri, uri::Authority};
use openssl::x509::X509;
use oxiroute_config::{
    AccessLogPolicy, AlpnProtocol, Certificate, CertificateSource, Config, ConfigError,
    DnsResolutionPolicy, HealthCheck, HealthCheckType, HealthHttpVersion, HttpAccessPolicy,
    HttpHostSelector, HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpService,
    HttpVersionPolicy, L4Service, Listener, ListenerBind, Protocol, RtmpApplication,
    RtmpPushTarget, RtmpRecorderStart, RtmpService, Stats, StatsPage, StatsPageAdminPolicy,
    TlsProfile, TlsVersion, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, UpstreamServer,
    UpstreamTls, load_lua,
};
use oxiroute_rtmp::{RtmpCapabilities, RtmpRegistry, StreamKey};
use oxiroute_server::{
    CertbotWatcherConfig, CertbotWatcherSupervisor, RtmpManagementApi, RuntimeEndpoint,
    RuntimeMetrics, ServiceKind, ServicePlanError, runtime_plan, service_specs,
};
use serde_json::Value;
use tempfile::TempDir;

use config_support::{
    empty_config, loopback_address as address, loopback_bind as socket_bind,
    loopback_endpoint as socket_endpoint, rtmp_recorder as recorder,
};
use fixture_support::{create_secure_root, write_file_with_mode, write_test_identity};

#[test]
fn startup_dns_cannot_resolve_to_a_statistics_listener() {
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: vec!["127.0.0.1:18404".parse().expect("stats bind")],
        admin_token_file: None,
        pages: Vec::new(),
    });
    config.upstream_pools.push(UpstreamPool {
        name: "protected".into(),
        servers: vec![UpstreamServer {
            name: "stats-rebind".into(),
            endpoint: UpstreamEndpoint::Dns {
                host: "localhost".into(),
                port: 18404,
            },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::Startup,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
    });

    let Err(error) = runtime_plan(&config) else {
        panic!("protected startup DNS must fail")
    };
    assert!(
        error
            .to_string()
            .contains("protected management or statistics listener")
    );
}

#[test]
fn upstream_socket_cannot_target_a_statistics_page_listener() {
    let page_bind = "127.0.0.1:18405".parse().expect("page bind");
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: page_bind,
            uri_prefix: "/stats".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
    });
    config.upstream_pools.push(UpstreamPool {
        name: "protected-page".into(),
        servers: vec![UpstreamServer {
            name: "page-loop".into(),
            endpoint: UpstreamEndpoint::Socket { address: page_bind },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
    });

    let Err(error) = runtime_plan(&config) else {
        panic!("protected statistics page destination must fail")
    };
    assert!(
        error
            .to_string()
            .contains("protected management or statistics listener")
    );
}

#[test]
fn distributed_example_compiles_into_an_active_runtime_plan() {
    let config = load_lua(include_str!("../../../oxiroute.example.lua"))
        .expect("distributed example configuration");

    let plan = runtime_plan(&config).expect("distributed example runtime plan");

    assert_eq!(plan.services.len(), 3);
    assert!(plan.health_supervisor.is_some());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one integration case keeps all three audited host pool shapes visible"
)]
fn whitebeast_hostrouter_and_phoenix_pool_shapes_compile_with_named_policy() {
    let haproxy_http_check = HealthCheck {
        kind: HealthCheckType::Http,
        interval_ms: 2_000,
        timeout_ms: 1_000,
        healthy_threshold: 2,
        unhealthy_threshold: 3,
        startup: oxiroute_config::HealthStartup::Checking,
        fast_interval_ms: Some(1_000),
        down_interval_ms: Some(5_000),
        host: None,
        path: Some("/healthz".into()),
        expected_status: Some(200),
        http_version: Some(HealthHttpVersion::Http10),
    };
    let dns_server = |name: String, port| UpstreamServer {
        endpoint: UpstreamEndpoint::Dns {
            host: format!("{name}.lan"),
            port,
        },
        name,
        max_connections: Some(100),
        dns_resolution: DnsResolutionPolicy::OnConnect,
    };
    let pool = |name: &str,
                servers: Vec<UpstreamServer>,
                algorithm,
                health_check: Option<HealthCheck>| UpstreamPool {
        name: name.into(),
        servers,
        endpoints: Vec::new(),
        algorithm,
        health_check,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: Some(5_000),
        connect_timeout_ms: Some(5_000),
        server_timeout_ms: Some(50_000),
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
    };
    let config = Config {
        max_connections: Some(4_096),
        upstream_pools: vec![
            pool(
                "whitebeast",
                vec![
                    dns_server("whitebeast01".into(), 3_000),
                    dns_server("whitebeast02".into(), 3_000),
                ],
                UpstreamAlgorithm::First,
                Some(haproxy_http_check.clone()),
            ),
            pool(
                "app_nodes",
                vec![
                    dns_server("app01".into(), 3_000),
                    dns_server("app02".into(), 3_000),
                ],
                UpstreamAlgorithm::LeastConnections,
                Some(haproxy_http_check.clone()),
            ),
            pool(
                "phoenix_nodes",
                (1..=8)
                    .map(|ordinal| dns_server(format!("phoenix{ordinal:02}"), 4_000))
                    .collect(),
                UpstreamAlgorithm::LeastConnections,
                Some(haproxy_http_check),
            ),
        ],
        ..empty_config()
    };

    let plan = runtime_plan(&config).expect("host-shaped runtime plan");
    assert_eq!(plan.max_connections, Some(4_096));
    assert!(plan.health_supervisor.is_some());
    assert_eq!(
        plan.pools
            .iter()
            .map(|pool| {
                let snapshot = pool.health_snapshot();
                (
                    snapshot.name,
                    snapshot.algorithm,
                    snapshot
                        .endpoints
                        .into_iter()
                        .map(|server| server.name)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "whitebeast".into(),
                "first",
                vec!["whitebeast01".into(), "whitebeast02".into()]
            ),
            (
                "app_nodes".into(),
                "least_connections",
                vec!["app01".into(), "app02".into()]
            ),
            (
                "phoenix_nodes".into(),
                "least_connections",
                (1..=8)
                    .map(|ordinal| format!("phoenix{ordinal:02}"))
                    .collect()
            ),
        ]
    );
}

#[test]
fn rejects_an_active_cache_policy_instead_of_silently_ignoring_it() {
    let config = load_lua(
        r#"
return {
  version = 1,
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8080" },
      protocol = "http",
      service = "web",
    },
  },
  cache_stores = {
    {
      name = "memory",
      type = "memory",
      max_bytes = 1048576,
      max_entries = 128,
      max_object_bytes = 65536,
    },
  },
  upstream_pools = {
    {
      name = "origin",
      endpoints = { { type = "socket", address = "127.0.0.1:3000" } },
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        {
          path = { kind = "segment_prefix", value = "/" },
          action = {
            type = "proxy",
            upstream_pool = "origin",
            policy = { cache = { store = "memory" } },
          },
        },
      },
    },
  },
}

"#,
    )
    .expect("canonical cache configuration");

    let error = match runtime_plan(&config) {
        Ok(_) => panic!("inactive cache runtime must fail closed"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ServicePlanError::CacheRuntimeUnavailable {
            service,
            route: 0
        } if service == "web"
    ));
}

#[test]
fn request_buffering_compiles_and_response_buffering_fails_closed() {
    let mut config = canonical_config();
    config.http_services[0].routes[0].policy.request_buffering = true;

    runtime_plan(&config).expect("request buffering has an active runtime");

    config.http_services[0].routes[0].policy.response_buffering = true;

    let error = match runtime_plan(&config) {
        Ok(_) => panic!("response buffering must not be silently ignored"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ServicePlanError::RuntimePolicyUnavailable {
            policy: "http_services[].routes[].policy.response_buffering"
        }
    ));

    config.http_services[0].routes[0].policy.response_buffering = false;
    config.http_services[0].routes[0]
        .policy
        .max_request_body_bytes = None;
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::RuntimePolicyUnavailable {
            policy: "http_services[].routes[].policy.unbounded_request_buffering"
        })
    ));
}

#[test]
fn runtime_rejects_duplicate_routes_until_importer_first_wins_has_resolved_them() {
    let mut config = canonical_config();
    let duplicate = config.http_services[0].routes[0].clone();
    config.http_services[0].routes.push(duplicate);

    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::InvalidConfig(error))
            if matches!(*error, ConfigError::DuplicateHttpRoute { .. })
    ));

    config.http_services[0].routes.pop();
    runtime_plan(&config).expect("importer-resolved first route only");
}

#[cfg(unix)]
#[test]
fn basic_auth_rejects_unsupported_htpasswd_hashes() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new().expect("htpasswd fixture");
    let unsupported = directory.path().join("unsupported.htpasswd");
    fs::write(&unsupported, "user:{SHA}VBPuJHI7uixaa6LQGWx4s+5GKNE=\n")
        .expect("unsupported htpasswd");
    fs::set_permissions(&unsupported, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    let mut config = canonical_config();
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: unsupported.clone(),
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::AccessPreflight { .. })
    ));
}

#[cfg(unix)]
#[test]
fn basic_auth_preflights_apr1_and_mixed_scheme_files() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new().expect("htpasswd fixture");
    let mut config = canonical_config();
    let apr1 = directory.path().join("apr1.htpasswd");
    fs::write(&apr1, "myName:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n").expect("APR1 htpasswd");
    fs::set_permissions(&apr1, fs::Permissions::from_mode(0o600)).expect("APR1 mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: apr1.clone(),
        realm: "private".into(),
    });
    runtime_plan(&config).expect("valid APR1 htpasswd");

    fs::set_permissions(&apr1, fs::Permissions::from_mode(0o640))
        .expect("group-readable APR1 mode");
    runtime_plan(&config).expect("group-readable APR1 htpasswd");

    fs::set_permissions(&apr1, fs::Permissions::from_mode(0o644))
        .expect("world-readable APR1 mode");
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    for (name, hash) in [
        ("long-salt", "$apr1$toolongsalt$HqJZimcKQFAMYayBlzkrA/"),
        ("short-digest", "$apr1$r31.....$HqJZimcKQFAMYayBlzkrA"),
        ("invalid-digest", "$apr1$r31.....$HqJZimcKQFAMYayBlzkrA!"),
        (
            "noncanonical-digest",
            "$apr1$r31.....$HqJZimcKQFAMYayBlzkrAA",
        ),
    ] {
        let malformed_apr1 = directory.path().join(format!("{name}.htpasswd"));
        fs::write(&malformed_apr1, format!("user:{hash}\n")).expect("malformed APR1 htpasswd");
        fs::set_permissions(&malformed_apr1, fs::Permissions::from_mode(0o600))
            .expect("malformed APR1 mode");
        config.http_services[0].routes[0].access_policy =
            Some(HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path: malformed_apr1,
                realm: "private".into(),
            });
        assert!(matches!(
            runtime_plan(&config),
            Err(ServicePlanError::AccessPreflight { .. })
        ));
    }

    let duplicate_apr1 = directory.path().join("duplicate-apr1.htpasswd");
    fs::write(
        &duplicate_apr1,
        concat!(
            "user:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
            "user:$apr1$hfT7jp2q$2F2Tht4XByp/xPQ4H4.vT0\n",
        ),
    )
    .expect("duplicate APR1 htpasswd");
    fs::set_permissions(&duplicate_apr1, fs::Permissions::from_mode(0o600))
        .expect("duplicate APR1 mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: duplicate_apr1,
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    let mixed_schemes = directory.path().join("mixed-schemes.htpasswd");
    fs::write(
        &mixed_schemes,
        concat!(
            "first:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
            "second:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        ),
    )
    .expect("mixed-scheme htpasswd");
    fs::set_permissions(&mixed_schemes, fs::Permissions::from_mode(0o600))
        .expect("mixed-scheme mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: mixed_schemes,
        realm: "private".into(),
    });
    runtime_plan(&config).expect("mixed-scheme htpasswd");
}

#[cfg(unix)]
#[test]
fn basic_auth_preflights_a_large_multi_user_htpasswd_file() {
    use std::{fmt::Write as _, os::unix::fs::PermissionsExt as _};

    let directory = TempDir::new().expect("htpasswd fixture");
    let path = directory.path().join("large.htpasswd");
    let mut contents = String::with_capacity(1_000_000);
    for index in 0..20_000 {
        writeln!(
            contents,
            "user-{index:05}:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/"
        )
        .expect("write htpasswd entry");
    }
    assert!(contents.len() < 1024 * 1024);
    fs::write(&path, contents).expect("large htpasswd");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");

    let mut config = canonical_config();
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: path,
        realm: "private".into(),
    });
    runtime_plan(&config).expect("large htpasswd preflight");
}

#[cfg(unix)]
#[test]
fn basic_auth_rejects_symlinks_and_excessive_bcrypt_work() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = TempDir::new().expect("htpasswd fixture");
    let mut config = canonical_config();
    let bcrypt = directory.path().join("bcrypt.htpasswd");
    fs::write(
        &bcrypt,
        "user:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
    )
    .expect("bcrypt htpasswd");
    fs::set_permissions(&bcrypt, fs::Permissions::from_mode(0o600)).expect("bcrypt mode");
    let link = directory.path().join("linked.htpasswd");
    symlink(&bcrypt, &link).expect("htpasswd symlink");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: link,
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    let excessive = directory.path().join("excessive.htpasswd");
    fs::write(
        &excessive,
        "user:$2y$13$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
    )
    .expect("excessive bcrypt htpasswd");
    fs::set_permissions(&excessive, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: excessive,
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    let mixed = directory.path().join("mixed-cost.htpasswd");
    fs::write(
        &mixed,
        concat!(
            "first:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
            "second:$2y$06$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        ),
    )
    .expect("mixed bcrypt htpasswd");
    fs::set_permissions(&mixed, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: mixed,
        realm: "private".into(),
    });
    runtime_plan(&config).expect("mixed-cost bcrypt htpasswd");
}

#[test]
fn compiles_shared_http_and_l4_service_plans() {
    let config = canonical_config();

    let services = service_specs(&config).expect("valid service plan");

    assert_eq!(services.len(), 4);
    assert_eq!(services[0].name, "web");
    assert_eq!(services[0].bind, socket_bind(8080));
    assert_eq!(services[0].max_connections, Some(500));
    assert!(services.iter().all(|service| service.tls.is_none()));

    let ServiceKind::Http(first_http) = &services[0].kind else {
        panic!("first service must be HTTP");
    };
    let ServiceKind::Http(second_http) = &services[1].kind else {
        panic!("second service must be HTTP");
    };
    assert!(Arc::ptr_eq(first_http, second_http));
    assert_eq!(first_http.upstream_io_timeout(), Duration::from_secs(15));
    assert_eq!(first_http.max_request_body_bytes(), Some(2 * 1024 * 1024));
    let authority = "api.example.com".parse::<Authority>().expect("authority");
    let uri = "/v1/items".parse::<Uri>().expect("URI");
    assert_eq!(
        first_http
            .select(Some(&authority), &uri, &Method::GET)
            .map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(address(3000)))
    );
    assert_eq!(
        second_http
            .select(Some(&authority), &uri, &Method::GET)
            .map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(address(3001)))
    );

    let ServiceKind::Tcp(l4) = &services[2].kind else {
        panic!("third service must be TCP");
    };
    assert_eq!(
        l4.select().map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(address(5432)))
    );
    assert_eq!(l4.policy().connect, Duration::from_secs(5));
    assert_eq!(l4.policy().idle, Some(Duration::from_secs(120)));
    assert_eq!(l4.policy().lifetime, Some(Duration::from_secs(600)));
    assert!(matches!(services[3].kind, ServiceKind::Rtmp(_)));
}

#[test]
fn rtmp_listeners_share_one_service_identity_catalog_and_hub() {
    let mut config = canonical_config();
    let mut second_listener = config.listeners[3].clone();
    second_listener.name = "live-backup".into();
    second_listener.bind = socket_bind(1936);
    config.listeners.push(second_listener);

    let services = service_specs(&config).expect("valid shared RTMP service plan");
    let ServiceKind::Rtmp(first_plan) = &services[3].kind else {
        panic!("first RTMP listener must use the RTMP service");
    };
    let ServiceKind::Rtmp(second_plan) = &services[4].kind else {
        panic!("second RTMP listener must use the RTMP service");
    };
    assert!(Arc::ptr_eq(first_plan, second_plan));

    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let first_runtime = first_plan
        .runtime(Arc::clone(&registry))
        .expect("first RTMP runtime");
    let second_runtime = second_plan
        .runtime(Arc::clone(&registry))
        .expect("second RTMP runtime");
    assert_eq!(first_runtime.service_id(), "live");
    assert_eq!(second_runtime.service_id(), "live");
    assert!(Arc::ptr_eq(
        first_runtime.registry(),
        second_runtime.registry()
    ));

    let key = StreamKey::new("live", "live", "camera");
    let _publisher = first_runtime
        .hub()
        .attach_publisher(key.clone())
        .expect("publisher on first listener");
    assert!(second_runtime.hub().has_publisher(&key));
}

#[test]
fn compiles_chunk_disabled_access_log_and_application_fanout_policy() {
    let mut config = canonical_config();
    let service = &mut config.rtmp_services[0];
    service.outbound_chunk_size = 8_192;
    service.access_log = Some(AccessLogPolicy::Disabled);
    service.applications[0].fanout = oxiroute_config::RtmpFanoutPolicy {
        max_subscribers: 7,
        max_queue_messages_per_subscriber: 11,
        max_queue_bytes_per_subscriber: 4_096,
    };

    let services = service_specs(&config).expect("lowered RTMP runtime policy");
    let ServiceKind::Rtmp(plan) = &services[3].kind else {
        panic!("RTMP service plan");
    };
    let limits = plan.hub().limits();
    assert_eq!(limits.max_subscribers, 7);
    assert_eq!(limits.max_subscribers_per_stream, 7);
    assert_eq!(limits.max_queue_messages_per_subscriber, 11);
    assert_eq!(limits.max_queue_bytes_per_subscriber, 4_096);
    assert_eq!(limits.max_fanout_bytes, 7 * 4_096);
}

#[test]
fn resolves_absent_push_port_without_connecting_and_rejects_direct_listener_loops() {
    let mut config = canonical_config();
    config.rtmp_services[0].applications[0]
        .push_targets
        .push(RtmpPushTarget {
            host: "127.0.0.1".into(),
            port: 1_936,
            application: "$name".into(),
        });
    service_specs(&config).expect("absent push destination is a runtime concern");

    config.rtmp_services[0].applications[0].push_targets[0].port = 1_935;
    assert!(matches!(
        service_specs(&config),
        Err(ServicePlanError::RtmpPushDirectLoop { target: 0, .. })
    ));
}

#[cfg(unix)]
#[test]
fn recorder_planning_is_read_only_and_runtime_activation_opens_the_store() {
    let root = TempDir::new().expect("recording root");
    let mut config = canonical_config();
    config.rtmp_services[0].applications[0]
        .recorders
        .push(recorder(
            "archive",
            RtmpRecorderStart::Continuous,
            root.path(),
        ));
    config.rtmp_services[0].applications[0]
        .recorders
        .push(recorder("clips", RtmpRecorderStart::Manual, root.path()));

    let plan = runtime_plan(&config).expect("recorder runtime plan");

    let listener = plan
        .topology
        .nodes()
        .iter()
        .find(|node| node.kind == oxiroute_server::TopologyNodeKind::RtmpListener)
        .expect("RTMP topology listener");
    assert_eq!(
        listener.attributes["applications"][0]["recording"]["recorderCount"],
        2
    );
    assert!(plan.rtmp_recording_supported);
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
    assert!(!root.path().join(".oxiroute-recording.lock").exists());
    let ServiceKind::Rtmp(service) = &plan.services[3].kind else {
        panic!("RTMP service plan");
    };
    service
        .runtime(Arc::new(RtmpRegistry::new(plan.rtmp_capabilities)))
        .expect("activated RTMP runtime");
    assert!(!root.path().join(".oxiroute-recording.lock").exists());
}

#[cfg(unix)]
#[test]
fn derives_manual_capability_from_canonical_recorder_policies() {
    let root = TempDir::new().expect("recording root");
    let mut continuous = canonical_config();
    continuous.rtmp_services[0].applications[0]
        .recorders
        .push(recorder(
            "archive",
            RtmpRecorderStart::Continuous,
            root.path(),
        ));
    let continuous_plan = runtime_plan(&continuous).expect("continuous recorder plan");
    assert!(continuous_plan.rtmp_capabilities.live_ingest);
    assert!(!continuous_plan.rtmp_capabilities.manual_recording);

    let mut manual = continuous;
    manual.rtmp_services[0].applications[0].recorders[0].start = RtmpRecorderStart::Manual;
    let manual_plan = runtime_plan(&manual).expect("manual recorder plan");
    assert!(manual_plan.rtmp_capabilities.live_ingest);
    assert!(manual_plan.rtmp_capabilities.manual_recording);
}

#[cfg(unix)]
#[test]
fn excludes_unreferenced_rtmp_services_from_active_capabilities() {
    let root = TempDir::new().expect("recording root");
    let mut config = canonical_config();
    config.rtmp_services.push(RtmpService {
        name: "orphan".into(),
        outbound_chunk_size: 4_096,
        access_log: None,
        applications: vec![RtmpApplication {
            name: "unused".into(),
            live: true,
            idle_streams: false,
            push_targets: Vec::new(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            recorders: vec![recorder(
                "manual-orphan",
                RtmpRecorderStart::Manual,
                root.path(),
            )],
        }],
    });

    let plan = runtime_plan(&config).expect("runtime plan with orphan RTMP service");

    assert!(plan.rtmp_capabilities.live_ingest);
    assert!(!plan.rtmp_capabilities.manual_recording);
    assert!(!plan.rtmp_recording_supported);
}

#[cfg(unix)]
#[test]
fn rejects_insecure_and_overquota_recording_roots_without_path_disclosure() {
    use std::os::unix::fs::PermissionsExt as _;

    let insecure_root = TempDir::new().expect("insecure recording root");
    fs::set_permissions(insecure_root.path(), fs::Permissions::from_mode(0o777))
        .expect("insecure root mode");
    let mut insecure = canonical_config();
    insecure.rtmp_services[0].applications[0]
        .recorders
        .push(recorder(
            "archive",
            RtmpRecorderStart::Continuous,
            insecure_root.path(),
        ));
    let error = match runtime_plan(&insecure) {
        Ok(_) => panic!("insecure root must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, ServicePlanError::RecorderPreflight { .. }));
    assert!(
        !error
            .to_string()
            .contains(&insecure_root.path().display().to_string())
    );

    let overquota_root = TempDir::new().expect("overquota recording root");
    fs::write(overquota_root.path().join("existing.flv"), b"over quota")
        .expect("overquota fixture");
    let mut overquota = canonical_config();
    let mut policy = recorder(
        "archive",
        RtmpRecorderStart::Continuous,
        overquota_root.path(),
    );
    policy.max_queue_bytes = 1;
    policy.max_storage_bytes = Some(1);
    overquota.rtmp_services[0].applications[0]
        .recorders
        .push(policy);
    let error = match runtime_plan(&overquota) {
        Ok(_) => panic!("overquota root must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, ServicePlanError::RecorderPreflight { .. }));
    assert!(
        !error
            .to_string()
            .contains(&overquota_root.path().display().to_string())
    );
}

#[test]
fn compiles_and_retains_one_shared_listener_tls_profile() {
    let temp = TempDir::new().expect("TLS temp directory");
    let (certificate_chain_path, private_key_path) =
        write_test_identity(temp.path(), "test-only-private-key.pem");
    let mut config = canonical_config();
    config.certificates.push(Certificate {
        name: "public".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Files {
            certificate_chain_path,
            private_key_path,
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "public".into(),
        certificates: vec!["public".into()],
        default_certificate: "public".into(),
        min_version: TlsVersion::Tls12,
        alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
    });
    config.listeners[0].tls_profile = Some("public".into());
    config.listeners[1].tls_profile = Some("public".into());

    let plan = runtime_plan(&config).expect("TLS runtime plan");
    let first = plan.services[0].tls.as_ref().expect("first TLS profile");
    let second = plan.services[1].tls.as_ref().expect("second TLS profile");
    let prepared = plan.tls.profiles().get("public").expect("prepared profile");

    assert!(Arc::ptr_eq(first, second));
    assert!(Arc::ptr_eq(first, prepared));
    assert_eq!(first.name(), "public");
    assert_eq!(first.min_version(), TlsVersion::Tls12);
    assert!(first.tls_settings().is_ok());
    assert_eq!(plan.tls.certificates().len(), 1);
    assert_eq!(plan.tls.profiles().len(), 1);
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn prepares_certbot_reconcilers_with_the_profile_active_generation() {
    let temp = TempDir::new().expect("Certbot temp directory");
    let (live, archive) = write_test_certbot_lineage(&temp);
    let mut config = canonical_config();
    config.certificates.push(Certificate {
        name: "public".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Certbot {
            live_directory_path: live,
            archive_directory_path: archive.clone(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "public".into(),
        certificates: vec!["public".into()],
        default_certificate: "public".into(),
        min_version: TlsVersion::Tls12,
        alpn: vec![AlpnProtocol::Http11],
    });
    config.listeners[0].tls_profile = Some("public".into());

    let plan = runtime_plan(&config).expect("Certbot runtime plan");
    let [reconciler] = plan.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be retained");
    };
    let active = plan
        .tls
        .certificates()
        .get("public")
        .expect("active Certbot identity");
    let initial = active.snapshot();

    assert_eq!(reconciler.active_archive_revision(), 1);
    assert!(Arc::ptr_eq(reconciler.active_generation(), active));
    write_test_certbot_revision(&archive, 2, "proxy-b.pem", "proxy-b-key.pem");
    assert_eq!(reconciler.active_archive_revision(), 1);
    assert!(Arc::ptr_eq(&active.snapshot(), &initial));

    let mut watcher = CertbotWatcherSupervisor::start(
        vec![Arc::clone(reconciler)],
        CertbotWatcherConfig::default(),
    )
    .expect("Certbot watcher");
    let metrics = RuntimeMetrics::new();
    metrics
        .register_certbot_monitoring([Arc::clone(reconciler)], Some(watcher.monitor()))
        .expect("Certbot monitoring");
    let startup_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while watcher.status().rescans == 0 && std::time::Instant::now() < startup_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(watcher.status().rescans > 0);

    set_test_certbot_revision(&archive, 2);
    let activation_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while reconciler.active_archive_revision() != 2
        && std::time::Instant::now() < activation_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(reconciler.active_archive_revision(), 2);
    assert!(!Arc::ptr_eq(&active.snapshot(), &initial));

    let api = RtmpManagementApi::new(
        Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        })),
        metrics.clone(),
        Arc::clone(&plan.topology),
    );
    let response = api.handle("GET", "/api/v1/monitoring", 100);
    let body: Value = serde_json::from_slice(&response.body).expect("monitoring JSON");
    let serialized = String::from_utf8(response.body).expect("UTF-8 monitoring JSON");

    assert_eq!(body["certbotCertificates"][0]["name"], "public");
    assert_eq!(body["certbotCertificates"][0]["activeArchiveRevision"], 2);
    assert!(
        body["certbotCertificates"][0]["activeContentRevision"]
            .as_str()
            .is_some_and(|revision| !revision.is_empty())
    );
    assert!(
        body["certbotCertificates"][0]["expiresAt"]
            .as_str()
            .is_some_and(|expiry| !expiry.is_empty())
    );
    assert_eq!(
        body["certbotCertificates"][0]["lastOutcome"],
        "activated_forward"
    );
    assert!(body["certbotCertificates"][0]["lastErrorCode"].is_null());
    assert_eq!(body["certbotWatcher"]["health"], "healthy");
    assert!(
        body["certbotWatcher"]["rescans"]
            .as_str()
            .is_some_and(|value| value.parse::<u64>().is_ok())
    );
    assert!(!serialized.contains("proxy.example.test"));
    assert!(!serialized.contains(&temp.path().display().to_string()));
    assert!(!serialized.contains("BEGIN CERTIFICATE"));
    assert!(!serialized.contains("PRIVATE KEY"));

    fs::write(archive.join("fullchain2.pem"), b"invalid candidate")
        .expect("corrupt Certbot candidate");
    let failure_deadline = std::time::Instant::now() + Duration::from_secs(3);
    let failed = loop {
        let snapshot = metrics.snapshot().expect("failed reconciliation snapshot");
        if snapshot.certbot_certificates[0].last_error_code.as_deref() == Some("invalid_candidate")
            && snapshot.certbot_watcher.as_ref().is_some_and(|watcher| {
                watcher.health == oxiroute_server::CertbotWatcherHealth::Degraded
            })
        {
            break snapshot;
        }
        assert!(
            std::time::Instant::now() < failure_deadline,
            "Certbot watcher did not propagate the invalid filesystem candidate"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(failed.certbot_certificates[0].active_archive_revision, 2);
    assert_eq!(failed.certbot_certificates[0].last_outcome, None);
    assert_eq!(
        failed.certbot_certificates[0].last_error_code.as_deref(),
        Some("invalid_candidate")
    );
    assert_eq!(
        failed
            .certbot_watcher
            .expect("degraded watcher snapshot")
            .health,
        oxiroute_server::CertbotWatcherHealth::Degraded
    );

    watcher.shutdown();
    assert_eq!(
        metrics
            .snapshot()
            .expect("stopped watcher snapshot")
            .certbot_watcher
            .expect("watcher snapshot")
            .health,
        oxiroute_server::CertbotWatcherHealth::Stopped
    );
}

#[test]
fn rejects_an_unknown_programmatic_listener_tls_profile() {
    let mut config = canonical_config();
    config.listeners[0].tls_profile = Some("missing".into());

    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::UnknownListenerTlsProfile { listener, profile }
                    if listener == "web" && profile == "missing"
            )
    ));
}

#[test]
fn rejects_tls_profiles_on_programmatic_tcp_and_rtmp_listeners() {
    for (listener_index, expected_protocol) in [(2, Protocol::Tcp), (3, Protocol::Rtmp)] {
        let mut config = canonical_config();
        config.listeners[listener_index].tls_profile = Some("not-allowed".into());

        assert!(matches!(
            runtime_plan(&config),
            Err(ServicePlanError::InvalidConfig(source))
                if matches!(
                    source.as_ref(),
                    ConfigError::UnexpectedListenerTlsProfile { protocol, .. }
                        if *protocol == expected_protocol
                )
        ));
    }
}

#[test]
fn rejects_a_tls_upstream_pool_for_a_programmatic_l4_service() {
    let mut config = canonical_config();
    config.upstream_pools[1].tls = Some(UpstreamTls {
        server_name: "database.example.test".into(),
        ca_certificate_path: None,
    });

    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::TlsUpstreamPoolForL4Service { service, pool }
                    if service == "database" && pool == "database"
            )
    ));
}

#[test]
fn rejects_an_invalid_programmatic_listener_without_panicking() {
    let mut config = canonical_config();
    config.listeners[0].service = None;

    assert!(matches!(
        service_specs(&config),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::MissingListenerService { listener, .. } if listener == "web"
            )
    ));
}

#[test]
fn rejects_an_invalid_programmatic_route_pool_reference() {
    let mut config = canonical_config();
    let HttpRouteAction::Proxy { upstream_pool, .. } =
        &mut config.http_services[0].routes[0].action
    else {
        panic!("test route must proxy");
    };
    *upstream_pool = "missing".into();

    assert!(matches!(
        service_specs(&config),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::UnknownRouteUpstreamPool { service, route: 0, pool }
                    if service == "api" && pool == "missing"
            )
    ));
}

#[test]
fn rejects_unvalidated_programmatic_certificate_paths() {
    let mut config = canonical_config();
    config.certificates.push(Certificate {
        name: "public".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Files {
            certificate_chain_path: "relative-chain.pem".into(),
            private_key_path: "/private/key.pem".into(),
        },
    });

    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::InvalidFilePath {
                    kind: "certificate",
                    name,
                    field: "source.certificate_chain_path",
                    ..
                } if name == "public"
            )
    ));
}

#[test]
fn refuses_to_discard_a_required_health_supervisor() {
    let mut config = canonical_config();
    config.upstream_pools[0].health_check = Some(HealthCheck {
        kind: HealthCheckType::Tcp,
        interval_ms: 1_000,
        timeout_ms: 100,
        healthy_threshold: 1,
        unhealthy_threshold: 1,
        startup: oxiroute_config::HealthStartup::default(),
        fast_interval_ms: None,
        down_interval_ms: None,
        host: None,
        path: None,
        expected_status: None,
        http_version: None,
    });

    assert!(matches!(
        service_specs(&config),
        Err(ServicePlanError::HealthSupervisorRequired)
    ));
}

#[test]
fn rejects_invalid_programmatic_pool_definitions() {
    let mut duplicate_name = canonical_config();
    duplicate_name.upstream_pools[1].name = "api".into();
    assert!(matches!(
        service_specs(&duplicate_name),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::DuplicateName { namespace: "upstream pool", name } if name == "api"
            )
    ));

    let mut duplicate_endpoint = canonical_config();
    duplicate_endpoint.upstream_pools[0].endpoints[1] = socket_endpoint(3000);
    assert!(matches!(
        service_specs(&duplicate_endpoint),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(source.as_ref(), ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "api")
    ));

    let mut zero_port = canonical_config();
    zero_port.upstream_pools[0].endpoints[0] = socket_endpoint(0);
    assert!(matches!(
        service_specs(&zero_port),
        Err(ServicePlanError::InvalidConfig(source))
            if matches!(
                source.as_ref(),
                ConfigError::ZeroPort { kind: "upstream pool", name, field: "endpoints" }
                    if name == "api"
            )
    ));
}

#[test]
fn preserves_unbounded_listener_and_request_body_limits() {
    let mut config = canonical_config();
    config.listeners[0].max_connections = None;
    config.http_services[0].max_request_body_bytes = None;

    let services = service_specs(&config).expect("unbounded service plan");
    assert_eq!(services[0].max_connections, None);
    let ServiceKind::Http(service) = &services[0].kind else {
        panic!("first service must be HTTP");
    };
    assert_eq!(service.max_request_body_bytes(), None);
}

#[cfg(unix)]
#[test]
fn runtime_preflight_preserves_every_endpoint_identity_without_connecting() {
    let mut config = canonical_config();
    config.listeners[2].bind = ListenerBind::Unix {
        path: "/tmp/oxiroute-listener-preflight-does-not-exist.sock".into(),
        mode: None,
    };
    config.upstream_pools[0].endpoints = vec![
        socket_endpoint(3000),
        UpstreamEndpoint::Dns {
            host: "not-resolved.invalid".into(),
            port: 3001,
        },
        UpstreamEndpoint::Unix {
            path: "/tmp/oxiroute-preflight-does-not-exist.sock".into(),
        },
    ];

    let plan = runtime_plan(&config).expect("typed endpoint preflight");
    assert_eq!(
        plan.services[2].bind,
        ListenerBind::Unix {
            path: "/tmp/oxiroute-listener-preflight-does-not-exist.sock".into(),
            mode: None,
        }
    );
    let endpoints = &plan.pools[0].health_snapshot().endpoints;
    assert_eq!(endpoints[0].address.to_string(), "127.0.0.1:3000");
    assert_eq!(
        endpoints[1].address.to_string(),
        "not-resolved.invalid:3001"
    );
    assert_eq!(
        endpoints[2].address.to_string(),
        "/tmp/oxiroute-preflight-does-not-exist.sock"
    );
}

#[cfg(unix)]
#[test]
fn http_access_and_static_preflight_is_read_only_secure_and_redacted() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = TempDir::new().expect("HTTP preflight directory");
    let root = create_secure_root(directory.path(), "public");
    fs::write(root.join("index.html"), b"index").expect("static fixture");
    let token = "preflight-token-0123456789abcdef0123456789abcdef";
    let token_path = write_file_with_mode(directory.path(), "route-token", token.as_bytes(), 0o600);
    let before_entries = fs::read_dir(&root)
        .expect("root entries")
        .map(|entry| entry.expect("root entry").file_name())
        .collect::<Vec<_>>();

    let mut config = canonical_config();
    config.http_services[0].routes[0].access_policy =
        Some(oxiroute_config::HttpAccessPolicy::BearerTokenFile {
            token_file_path: token_path.clone(),
            header_name: "authorization".into(),
            realm: None,
        });
    config.http_services[0].routes[0].action = HttpRouteAction::StaticFiles {
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
    };

    runtime_plan(&config).expect("secure read-only HTTP preflight");
    let after_entries = fs::read_dir(&root)
        .expect("root entries after preflight")
        .map(|entry| entry.expect("root entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(before_entries, after_entries);
    assert_eq!(
        fs::read_to_string(&token_path).expect("unchanged token"),
        token
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644))
        .expect("insecure token mode");
    let error = match runtime_plan(&config) {
        Ok(_) => panic!("insecure token mode must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, ServicePlanError::AccessPreflight { .. }));
    let diagnostic = error.to_string();
    assert!(!diagnostic.contains(token));
    assert!(!diagnostic.contains(&token_path.display().to_string()));

    let real_token = write_file_with_mode(directory.path(), "real-token", token.as_bytes(), 0o600);
    fs::remove_file(&token_path).expect("remove insecure token");
    symlink(&real_token, &token_path).expect("token symlink");
    assert!(matches!(
        runtime_plan(&config),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    config.http_services[0].routes[0].access_policy = None;
    let moved_root = directory.path().join("real-public");
    fs::rename(&root, &moved_root).expect("move static root");
    symlink(&moved_root, &root).expect("static root symlink");
    let error = match runtime_plan(&config) {
        Ok(_) => panic!("symlink static root must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, ServicePlanError::StaticPreflight { .. }));
    assert!(!error.to_string().contains(&root.display().to_string()));
}

#[allow(clippy::too_many_lines)]
fn canonical_config() -> Config {
    Config {
        listeners: vec![
            Listener {
                name: "web".into(),
                bind: socket_bind(8080),
                protocol: Protocol::Http,
                service: Some("api".into()),
                tls_profile: None,
                max_connections: Some(500),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            },
            Listener {
                name: "web-alt".into(),
                bind: socket_bind(8081),
                protocol: Protocol::Http,
                service: Some("api".into()),
                tls_profile: None,
                max_connections: Some(250),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            },
            Listener {
                name: "database".into(),
                bind: socket_bind(15432),
                protocol: Protocol::Tcp,
                service: Some("database".into()),
                tls_profile: None,
                max_connections: Some(100),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            },
            Listener {
                name: "live".into(),
                bind: socket_bind(1935),
                protocol: Protocol::Rtmp,
                service: Some("live".into()),
                tls_profile: None,
                max_connections: Some(50),
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            },
        ],
        upstream_pools: vec![
            UpstreamPool {
                name: "api".into(),
                servers: Vec::new(),
                endpoints: vec![socket_endpoint(3000), socket_endpoint(3001)],
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
            },
            UpstreamPool {
                name: "database".into(),
                servers: Vec::new(),
                endpoints: vec![socket_endpoint(5432)],
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
                    value: "api.example.com".into(),
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
            upstream_io_timeout_ms: 15_000,
            max_request_body_bytes: Some(2 * 1024 * 1024),
            gzip: None,
            access_log: None,
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
            outbound_chunk_size: 4_096,
            access_log: None,
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: true,
                push_targets: Vec::new(),
                fanout: oxiroute_config::RtmpFanoutPolicy::default(),
                recorders: Vec::new(),
            }],
        }],
        l4_services: vec![L4Service {
            name: "database".into(),
            upstream_pool: "database".into(),
            connect_timeout_ms: 5_000,
            idle_timeout_ms: 120_000,
            lifetime_timeout_ms: Some(600_000),
        }],
        ..empty_config()
    }
}

#[cfg(unix)]
fn write_test_certbot_lineage(temp: &TempDir) -> (PathBuf, PathBuf) {
    let live = temp.path().join("live/public");
    let archive = temp.path().join("archive/public");
    fs::create_dir_all(&live).expect("create Certbot live directory");
    fs::create_dir_all(&archive).expect("create Certbot archive directory");
    write_test_certbot_revision(&archive, 1, "proxy-a.pem", "proxy-a-key.pem");
    set_test_certbot_revision(&archive, 1);
    (live, archive)
}

#[cfg(unix)]
fn write_test_certbot_revision(
    archive: &Path,
    revision: u64,
    chain_fixture: &str,
    key_fixture: &str,
) {
    use std::os::unix::fs::PermissionsExt as _;

    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let fullchain = fs::read(fixtures.join(chain_fixture)).expect("read test fullchain");
    let certificates = X509::stack_from_pem(&fullchain).expect("parse test fullchain");
    let cert = certificates[0].to_pem().expect("encode test leaf");
    let chain = certificates[1..]
        .iter()
        .flat_map(|certificate| certificate.to_pem().expect("encode test issuer"))
        .collect::<Vec<_>>();
    fs::write(archive.join(format!("cert{revision}.pem")), cert).expect("write Certbot leaf");
    fs::write(archive.join(format!("chain{revision}.pem")), chain).expect("write Certbot chain");
    fs::write(archive.join(format!("fullchain{revision}.pem")), fullchain)
        .expect("write Certbot fullchain");
    let key_path = archive.join(format!("privkey{revision}.pem"));
    fs::write(
        &key_path,
        fs::read(fixtures.join(key_fixture)).expect("read test private key"),
    )
    .expect("write Certbot private key");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
        .expect("secure Certbot private key");
}

#[cfg(unix)]
fn set_test_certbot_revision(archive: &Path, revision: u64) {
    use std::os::unix::fs::symlink;

    let live = archive
        .parent()
        .and_then(Path::parent)
        .expect("archive root")
        .join("live/public");
    for stem in ["cert", "chain", "fullchain", "privkey"] {
        let link = live.join(format!("{stem}.pem"));
        if fs::symlink_metadata(&link).is_ok() {
            fs::remove_file(&link).expect("remove prior Certbot live link");
        }
        symlink(
            Path::new("../../archive/public").join(format!("{stem}{revision}.pem")),
            link,
        )
        .expect("write Certbot live link");
    }
}
