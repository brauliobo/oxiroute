#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn expired_preparation_deadline_drops_resources_without_quarantining_revision() {
    let manager = GenerationManager::new();
    let prepared_document = document();

    let Err(error) = manager.prepare_with_deadline(prepared_document, Instant::now()) else {
        panic!("expired preparation deadline succeeded")
    };

    assert!(matches!(error, GenerationError::PreparationTimedOut));
    let status = manager.status();
    assert_eq!(status.quarantined_revision, None);
    assert_eq!(status.last_failure, Some("generation_prepare_timeout"));
    assert!(status.disk_revision.is_some());
    assert!(manager.candidate().is_none());
    assert!(manager.prepare(document()).is_ok());
}

#[test]
fn contended_preparation_deadline_expires_without_candidate_or_quarantine_and_retries() {
    let manager = GenerationManager::new();
    let authority = manager.operations.acquire();
    let deadline = Instant::now() + Duration::from_millis(25);

    let Err(error) = manager.prepare_with_deadline(document(), deadline) else {
        panic!("contended preparation succeeded")
    };

    assert!(Instant::now() >= deadline);
    assert!(matches!(error, GenerationError::PreparationTimedOut));
    let status = manager.status();
    assert_eq!(status.candidate_revision, None);
    assert_eq!(status.quarantined_revision, None);
    assert_eq!(status.last_failure, Some("generation_prepare_timeout"));
    assert_eq!(status.failures, 1);
    drop(authority);
    assert!(manager.prepare(document()).is_ok());
}

#[test]
fn disk_registry_wait_timeout_is_retryable_without_quarantine() {
    let directory = TempDir::new().expect("disk cache root");
    let root = directory.path().join("cache");
    let (hook, reached, release) =
        crate::service_plan::install_disk_registry_opening_hook(root.clone());
    let opener_manager = GenerationManager::new();
    let opener_thread_manager = opener_manager.clone();
    let opener_document = disk_cache_document(&root);
    let opener = thread::spawn(move || opener_thread_manager.prepare(opener_document));
    reached.wait();

    let manager = GenerationManager::new();
    let deadline = Instant::now() + Duration::from_millis(25);
    let Err(error) = manager.prepare_with_deadline(disk_cache_document(&root), deadline) else {
        panic!("disk registry waiter did not time out")
    };

    assert!(matches!(error, GenerationError::PreparationTimedOut));
    assert!(Instant::now() >= deadline);
    let status = manager.status();
    assert_eq!(status.candidate_revision, None);
    assert_eq!(status.quarantined_revision, None);
    assert_eq!(status.last_failure, Some("generation_prepare_timeout"));
    assert_eq!(status.failures, 1);

    release.wait();
    opener
        .join()
        .expect("disk backend opener thread")
        .expect("disk backend opener generation");
    drop(hook);
    manager
        .prepare(disk_cache_document(&root))
        .expect("disk registry timeout retry");
}

#[test]
fn ordinary_preparation_failure_quarantines_even_after_deadline() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let address = occupied.local_addr().expect("occupied address");
    let mut config = document().validated_config.to_draft();
    let service = serde_json::from_value(serde_json::json!({
        "name": "occupied-service",
        "routes": [{
            "path": {"kind": "segment_prefix", "value": "/"},
            "action": {"type": "fixed_response", "status": 200}
        }]
    }))
    .expect("occupied service");
    config.http_services.push(service);
    config.listeners.push(oxiroute_config::Listener {
        name: "occupied".into(),
        bind: oxiroute_config::ListenerBind::Socket { address },
        protocol: oxiroute_config::Protocol::Http,
        service: Some("occupied-service".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    let document = document_for(&config);
    let revision = document.effective_revision.clone();

    let manager = GenerationManager::new();
    let Err(error) = manager.prepare_with_deadline(document, Instant::now()) else {
        panic!("occupied listener preparation succeeded")
    };
    assert!(matches!(error, GenerationError::Listener(_)));
    let message = error.to_string();
    assert!(message.contains("could not bind socket"), "{message}");
    assert!(message.contains(&address.to_string()), "{message}");
    assert_eq!(manager.status().quarantined_revision, Some(revision));
}

#[cfg(target_os = "linux")]
#[test]
fn expired_adopted_preparation_deadline_releases_adopted_descriptors() {
    let root = TempDir::new().expect("adopted deadline root");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted listener");
    let address = listener.local_addr().expect("adopted address");
    let (mut document, descriptors) = adopted_rtmp_fixture(root.path(), address, listener, false);
    let mut config = document.validated_config.to_draft();
    config.rtmp_services[0].applications[0].recorders.clear();
    document.validated_config = config.validate().expect("recording-free adopted config");
    let manager = GenerationManager::new_supervised();

    let Err(error) = manager.prepare_adopted_with_deadline(document, descriptors, Instant::now())
    else {
        panic!("expired adopted preparation succeeded")
    };

    assert!(matches!(error, GenerationError::PreparationTimedOut));
    assert_eq!(manager.process.listener_count(), 0);
    assert!(manager.status().quarantined_revision.is_none());
    std::net::TcpListener::bind(address).expect("adopted descriptor was released");
}

#[cfg(target_os = "linux")]
#[test]
fn contended_adopted_preparation_deadline_releases_descriptors_and_retries() {
    let root = TempDir::new().expect("adopted contention root");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted listener");
    let address = listener.local_addr().expect("adopted address");
    let (mut document, descriptors) = adopted_rtmp_fixture(root.path(), address, listener, false);
    let mut config = document.validated_config.to_draft();
    config.rtmp_services[0].applications[0].recorders.clear();
    document.validated_config = config.validate().expect("recording-free adopted config");
    let manager = GenerationManager::new_supervised();
    let authority = manager.operations.acquire();

    let Err(error) = manager.prepare_adopted_with_deadline(
        document,
        descriptors,
        Instant::now() + Duration::from_millis(25),
    ) else {
        panic!("contended adopted preparation succeeded")
    };

    assert!(matches!(error, GenerationError::PreparationTimedOut));
    assert!(manager.candidate().is_none());
    assert!(manager.status().quarantined_revision.is_none());
    let retry_listener =
        std::net::TcpListener::bind(address).expect("timed-out adoption released descriptor");
    let (mut retry_document, retry_descriptors) =
        adopted_rtmp_fixture(root.path(), address, retry_listener, false);
    let mut retry_config = retry_document.validated_config.to_draft();
    retry_config.rtmp_services[0].applications[0]
        .recorders
        .clear();
    retry_document.validated_config = retry_config
        .validate()
        .expect("recording-free retry config");
    drop(authority);
    assert!(
        manager
            .prepare_adopted(retry_document, retry_descriptors)
            .is_ok()
    );
}

#[test]
fn expired_rollback_deadline_is_retryable_without_quarantine() {
    let manager = GenerationManager::new();
    let first = manager
        .prepare(document_with_max_connections(Some(1)))
        .expect("first");
    manager.activate(&first).expect("first activation");
    let second = manager
        .prepare(document_with_max_connections(Some(2)))
        .expect("second");
    manager.activate(&second).expect("second activation");

    let Err(error) = manager.rollback_with_deadline(Instant::now()) else {
        panic!("expired rollback preparation succeeded")
    };

    assert!(matches!(error, GenerationError::PreparationTimedOut));
    assert!(manager.candidate().is_none());
    assert!(manager.status().quarantined_revision.is_none());
    assert!(manager.rollback().is_ok());
}
