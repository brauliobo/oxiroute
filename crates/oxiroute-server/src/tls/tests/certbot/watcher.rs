#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(target_os = "linux")]
#[test]
fn production_watcher_reconciles_the_preparation_to_startup_change_immediately() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    write_new_certbot_revision(temp.path(), &files, 2, "startup", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);
    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let [reconciler] = prepared.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be prepared");
    };

    // This activation happens after preparation and before the notify backend is installed, so it
    // cannot produce an event for the watcher. Startup reconciliation must close that gap.
    set_live_revision(&files, 2);
    let mut supervisor = prepared
        .start_certbot_watcher(CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(30),
            event_debounce: std::time::Duration::from_millis(10),
            event_max_delay: std::time::Duration::from_millis(50),
        })
        .unwrap()
        .expect("configured production watcher");

    wait_for_condition("startup reconciliation", || {
        reconciler.active_archive_revision() == 2
    });
    assert_eq!(supervisor.status().periodic_rescans, 0);
    assert!(supervisor.status().rescans > 0);
    supervisor.shutdown();
}

#[cfg(target_os = "linux")]
#[test]
fn production_watcher_reports_reconciliation_degradation_and_filesystem_recovery() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);
    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();
    let [reconciler] = prepared.certbot_reconcilers() else {
        panic!("one Certbot reconciler must be prepared");
    };
    let fullchain_path = files.archive.join("fullchain1.pem");
    let valid_fullchain = fs::read(&fullchain_path).unwrap();
    let mut supervisor = prepared
        .start_certbot_watcher(CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(30),
            event_debounce: std::time::Duration::from_millis(10),
            event_max_delay: std::time::Duration::from_millis(50),
        })
        .unwrap()
        .expect("configured production watcher");
    wait_for_condition("initial watcher reconciliation", || {
        supervisor.status().rescans > 0
    });

    fs::write(&fullchain_path, b"invalid replacement").unwrap();
    wait_for_condition("degraded reconciliation status", || {
        reconciler.status().last_error_code == Some("invalid_candidate")
            && supervisor.status().degraded
    });
    assert!(supervisor.status().reconciliation_failures > 0);
    assert_eq!(reconciler.active_archive_revision(), 1);

    fs::write(&fullchain_path, valid_fullchain).unwrap();
    wait_for_condition("reconciliation recovery", || {
        reconciler.status().last_error_code.is_none() && !supervisor.status().degraded
    });
    assert_eq!(reconciler.status().last_outcome, Some("unchanged"));
    assert_eq!(reconciler.active_archive_revision(), 1);
    supervisor.shutdown();
}

#[cfg(unix)]
#[test]
fn watcher_start_rejects_a_lineage_path_replaced_by_a_regular_file() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let config = config_with_certbot_lineage(&files);
    let prepared = prepare_tls(&config.clone().validate().expect("valid TLS test config")).unwrap();

    fs::remove_dir_all(&files.live).unwrap();
    fs::write(&files.live, b"not a lineage directory").unwrap();
    let error = prepared
        .start_certbot_watcher(CertbotWatcherConfig::default())
        .unwrap_err();

    assert!(matches!(
        error,
        CertbotWatcherError::PathNotDirectory { path } if path == files.live
    ));
}

#[test]
fn optional_certbot_watcher_does_not_start_without_identities() {
    assert!(
        CertbotWatcherSupervisor::start_if_configured(Vec::new(), CertbotWatcherConfig::default())
            .unwrap()
            .is_none()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_notify_ignores_loader_access_events_and_stable_lineage_settles() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = Arc::new(CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        active,
    ));
    let mut supervisor = CertbotWatcherSupervisor::start(
        vec![reconciler],
        CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(5),
            event_debounce: std::time::Duration::from_millis(25),
            event_max_delay: std::time::Duration::from_millis(150),
        },
    )
    .unwrap();

    set_live_revision(&files, 1);
    wait_for_condition("initial notify reconciliation", || {
        supervisor.status().rescans > 0
    });
    let mut settled_rescans = supervisor.status().rescans;
    let mut quiet_since = std::time::Instant::now();
    while quiet_since.elapsed() < std::time::Duration::from_millis(250) {
        let rescans = supervisor.status().rescans;
        if rescans != settled_rescans {
            settled_rescans = rescans;
            quiet_since = std::time::Instant::now();
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(supervisor.status().ignored_access_events > 0);

    thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(supervisor.status().rescans, settled_rescans);
    supervisor.shutdown();
}

#[cfg(target_os = "linux")]
#[test]
fn linux_notify_rebuilds_watches_after_lineage_directory_replacement() {
    let temp = TempDir::new().unwrap();
    let files = write_certbot_lineage(temp.path(), "primary", "certbot.example.test");
    let lineage = CertbotLineage::new(files.live.clone(), files.archive.clone());
    let initial = lineage
        .load_candidate("primary", &["certbot.example.test".into()])
        .unwrap();
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(
        initial.into_generation(),
    )));
    let reconciler = Arc::new(CertbotReconciler::new(
        lineage,
        "primary",
        vec!["certbot.example.test".into()],
        1,
        Arc::clone(&active),
    ));
    let mut supervisor = CertbotWatcherSupervisor::start(
        vec![Arc::clone(&reconciler)],
        CertbotWatcherConfig {
            rescan_interval: std::time::Duration::from_secs(5),
            event_debounce: std::time::Duration::from_millis(25),
            event_max_delay: std::time::Duration::from_millis(150),
        },
    )
    .unwrap();

    let replacement = CertbotTestLineage {
        name: "primary".into(),
        live: temp.path().join("live/primary-replacement"),
        archive: temp.path().join("archive/primary-replacement"),
    };
    fs::create_dir(&replacement.live).unwrap();
    fs::create_dir(&replacement.archive).unwrap();
    write_new_certbot_revision(
        temp.path(),
        &replacement,
        1,
        "replacement-one",
        "certbot.example.test",
    );
    write_new_certbot_revision(
        temp.path(),
        &replacement,
        2,
        "replacement-two",
        "certbot.example.test",
    );
    set_live_revision(&replacement, 2);

    fs::rename(&files.archive, temp.path().join("archive/primary-old")).unwrap();
    fs::rename(&replacement.archive, &files.archive).unwrap();
    fs::rename(&files.live, temp.path().join("live/primary-old")).unwrap();
    fs::rename(&replacement.live, &files.live).unwrap();

    wait_for_condition("replacement revision activation", || {
        reconciler.active_archive_revision() == 2
    });
    // Republish the same links and wait for a later refresh before testing the rebuilt watch.
    let watch_refreshes_before_barrier = supervisor.status().watch_refreshes;
    set_live_revision(&files, 2);
    wait_for_condition("watch rebuild after directory replacement", || {
        reconciler.active_archive_revision() == 2
            && supervisor.status().watch_refreshes > watch_refreshes_before_barrier
    });
    assert!(!supervisor.status().degraded);
    let replacement_two = active.snapshot();
    let rescans_after_replacement = supervisor.status().rescans;

    set_live_revision(&files, 1);
    wait_for_condition("event-driven rollback after watch rebuild", || {
        reconciler.active_archive_revision() == 1
            && supervisor.status().rescans > rescans_after_replacement
    });
    assert!(!Arc::ptr_eq(&active.snapshot(), &replacement_two));
    assert!(!supervisor.status().degraded);
    supervisor.shutdown();
}
