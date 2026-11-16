use super::*;

#[cfg(target_os = "linux")]
#[test]
fn direct_file_status_reports_rotation_failures_without_secret_material() {
    let temp = TempDir::new().expect("TLS temp directory");
    let (chain, key) = write_test_identity(temp.path(), "direct-status-key.pem");
    let mut config = canonical_config();
    config.certificates.push(Certificate {
        name: "public".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Files {
            certificate_chain_path: chain.clone(),
            private_key_path: key.clone(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "public".into(),
        certificates: vec!["public".into()],
        default_certificate: "public".into(),
        min_version: TlsVersion::Tls12,
        alpn: vec![AlpnProtocol::Http11],
        policy: oxiroute_config::TlsPolicy::default(),
    });

    let validated = config.validate().unwrap();
    let plan = runtime_plan(&validated).expect("direct-file runtime plan");
    let tls = oxiroute_server::tls::prepare_tls(&validated).expect("direct-file TLS");
    let [reconciler] = tls.file_reconcilers() else {
        panic!("one direct-file reconciler must be retained");
    };
    let initial_revision = reconciler.status().active_content_revision.clone();
    let mut watcher = FileWatcherSupervisor::start(
        vec![Arc::clone(reconciler)],
        FileWatcherConfig {
            rescan_interval: Duration::from_secs(30),
            event_debounce: Duration::from_millis(10),
            event_max_delay: Duration::from_millis(50),
        },
    )
    .expect("direct-file watcher");
    let metrics = RuntimeMetrics::new();
    metrics
        .register_direct_file_monitoring([Arc::clone(reconciler)], Some(watcher.monitor()))
        .expect("direct-file monitoring");

    let startup_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while watcher.status().rescans == 0 && std::time::Instant::now() < startup_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(watcher.status().rescans > 0);

    fs::write(&chain, b"invalid direct-file candidate").expect("corrupt direct-file candidate");
    let failure_deadline = std::time::Instant::now() + Duration::from_secs(3);
    let failed = loop {
        let snapshot = metrics.snapshot().expect("direct-file failure snapshot");
        if snapshot.direct_file_certificates[0]
            .last_error_code
            .as_deref()
            == Some("invalid_candidate")
            && snapshot
                .direct_file_watcher
                .as_ref()
                .is_some_and(|watcher| {
                    watcher.health == oxiroute_server::CertbotWatcherHealth::Degraded
                })
        {
            break snapshot;
        }
        assert!(
            std::time::Instant::now() < failure_deadline,
            "direct-file watcher did not propagate the invalid candidate"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        failed.direct_file_certificates[0].active_content_revision,
        initial_revision
    );
    let api = RtmpManagementApi::new(empty_rtmp_control(), metrics, Arc::clone(&plan.topology));
    let response = api.handle("GET", "/api/v1/monitoring", 100);
    let serialized = String::from_utf8(response.body).expect("monitoring JSON");
    assert!(!serialized.contains("proxy.example.test"));
    assert!(!serialized.contains(&temp.path().display().to_string()));
    assert!(!serialized.contains("BEGIN CERTIFICATE"));
    assert!(!serialized.contains("PRIVATE KEY"));

    watcher.shutdown();
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
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.listeners[0].tls_profile = Some("public".into());

    let validated = config.validate().unwrap();
    let plan = runtime_plan(&validated).expect("Certbot runtime plan");
    let tls = oxiroute_server::tls::prepare_tls(&validated).expect("Certbot TLS");
    let [reconciler] = tls.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be retained");
    };
    let active = tls
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
        empty_rtmp_control(),
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
