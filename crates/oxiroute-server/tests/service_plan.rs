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
    AlpnProtocol, Certificate, CertificateSource, Config, ConfigError, HealthCheck,
    HealthCheckType, HttpHostSelector, HttpPathSelector, HttpProxyPolicy, HttpRoute,
    HttpRouteAction, HttpService, HttpVersionPolicy, L4Service, Listener, ListenerBind, Protocol,
    RtmpApplication, RtmpRecorderStart, RtmpService, TlsProfile, TlsVersion, UpstreamAlgorithm,
    UpstreamEndpoint, UpstreamPool, UpstreamTls,
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
    assert!(root.path().join(".oxiroute-recording.lock").is_file());
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
        applications: vec![RtmpApplication {
            name: "unused".into(),
            live: true,
            idle_streams: false,
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
    policy.max_storage_bytes = 1;
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
    assert!(body["certbotWatcher"]["rescans"].as_u64().is_some());
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
        host: None,
        path: None,
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
        index_files: vec!["index.html".into()],
        spa_fallback: None,
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
            },
            Listener {
                name: "web-alt".into(),
                bind: socket_bind(8081),
                protocol: Protocol::Http,
                service: Some("api".into()),
                tls_profile: None,
                max_connections: Some(250),
            },
            Listener {
                name: "database".into(),
                bind: socket_bind(15432),
                protocol: Protocol::Tcp,
                service: Some("database".into()),
                tls_profile: None,
                max_connections: Some(100),
            },
            Listener {
                name: "live".into(),
                bind: socket_bind(1935),
                protocol: Protocol::Rtmp,
                service: Some("live".into()),
                tls_profile: None,
                max_connections: Some(50),
            },
        ],
        upstream_pools: vec![
            UpstreamPool {
                name: "api".into(),
                endpoints: vec![socket_endpoint(3000), socket_endpoint(3001)],
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
            },
            UpstreamPool {
                name: "database".into(),
                endpoints: vec![socket_endpoint(5432)],
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
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
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: true,
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
