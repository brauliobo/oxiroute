#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
#[test]
fn unix_reservation_reload_reuses_only_an_identical_path_and_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let path = directory.path().join("reload.sock");
    let mut first_config = empty_config();
    first_config.listeners.push(oxiroute_config::Listener {
        name: "unix".into(),
        bind: ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o600),
        },
        protocol: oxiroute_config::Protocol::Http,
        service: Some("http".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    let first = prepare_reservations(&first_config, None).expect("first Unix bind");
    let unchanged =
        prepare_reservations(&first_config, Some(&first)).expect("identical Unix bind reuse");
    assert!(Arc::ptr_eq(
        &first.get("unix").expect("first").inner,
        &unchanged.get("unix").expect("unchanged").inner,
    ));

    let mut changed_config = first_config.clone();
    changed_config.listeners[0].bind = ListenerBind::Unix {
        path: path.clone(),
        mode: Some(0o660),
    };
    let validated_config =
        validated_test_config(&changed_config).expect("valid changed Unix config");
    let validated = ListenerReservations::prepare_for_validation(&validated_config, Some(&first))
        .expect("validation reuses the active Unix path");
    assert!(Arc::ptr_eq(
        &first.get("unix").expect("first").inner,
        &validated.get("unix").expect("validated").inner,
    ));
    assert!(ListenerReservations::listener_restart_required(
        crate::RuntimeMode::Direct,
        &validated_test_config(&first_config).expect("valid first config"),
        &validated_config,
    ));
    changed_config.listeners[0].max_connections = Some(7);
    assert!(ListenerReservations::listener_restart_required(
        crate::RuntimeMode::Direct,
        &validated_test_config(&first_config).expect("valid first config"),
        &validated_test_config(&changed_config).expect("valid changed config"),
    ));
    assert!(prepare_reservations(&changed_config, Some(&first)).is_err());
    assert_eq!(
        std::fs::metadata(path)
            .expect("reserved socket metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "a changed mode must not silently reuse the old reservation"
    );
}

#[cfg(unix)]
#[test]
fn unix_reservation_reclaims_only_a_stale_socket() {
    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let stale_path = directory.path().join("stale.sock");
    std::os::unix::net::UnixListener::bind(&stale_path).expect("stale Unix bind");

    let reservation = ListenerReservation::bind(
        "stale",
        &ListenerBind::Unix {
            path: stale_path.clone(),
            mode: Some(0o600),
        },
    )
    .expect("stale socket reclaimed");
    assert_eq!(
        reservation.bind_text(),
        stale_path.to_str().expect("UTF-8 path")
    );
    let concurrent = ListenerReservation::bind(
        "concurrent",
        &ListenerBind::Unix {
            path: stale_path.clone(),
            mode: Some(0o600),
        },
    )
    .err()
    .expect("the active reservation owns the Unix namespace");
    assert_eq!(concurrent.kind(), io::ErrorKind::AddrInUse);

    let active_path = directory.path().join("active.sock");
    let active = std::os::unix::net::UnixListener::bind(&active_path).expect("active Unix bind");
    assert!(
        ListenerReservation::bind(
            "active",
            &ListenerBind::Unix {
                path: active_path.clone(),
                mode: None,
            },
        )
        .is_err()
    );
    assert!(active_path.exists());
    drop(active);
}

#[cfg(unix)]
#[test]
fn restrictive_stale_socket_is_preserved_when_liveness_cannot_be_proven() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let path = directory.path().join("restrictive.sock");
    let lease = UnixSocketLease::acquire(&path).expect("Unix namespace lease");
    drop(lease);

    let stale = std::os::unix::net::UnixListener::bind(&path).expect("stale Unix bind");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
        .expect("restrict stale Unix socket");
    drop(stale);

    let error = ListenerReservation::bind(
        "restrictive",
        &ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o400),
        },
    )
    .err()
    .expect("permission-denied stale socket must fail closed");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(path.exists());
}

#[cfg(unix)]
#[test]
fn unix_reservation_rejects_an_unprotected_writable_parent() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let unsafe_directory = directory.path().join("shared");
    std::fs::create_dir(&unsafe_directory).expect("shared Unix socket directory");
    std::fs::set_permissions(&unsafe_directory, std::fs::Permissions::from_mode(0o770))
        .expect("writable shared directory");
    let path = unsafe_directory.join("listener.sock");

    let error = ListenerReservation::bind(
        "unsafe",
        &ListenerBind::Unix {
            path,
            mode: Some(0o660),
        },
    )
    .err()
    .expect("non-sticky writable parent must fail closed");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

    let protected_directory = unsafe_directory.join("private");
    std::fs::create_dir(&protected_directory).expect("private Unix socket directory");
    std::fs::set_permissions(&protected_directory, std::fs::Permissions::from_mode(0o700))
        .expect("private socket directory");
    let error = ListenerReservation::bind(
        "unsafe-ancestor",
        &ListenerBind::Unix {
            path: protected_directory.join("listener.sock"),
            mode: Some(0o600),
        },
    )
    .err()
    .expect("writable ancestor must fail closed");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}
