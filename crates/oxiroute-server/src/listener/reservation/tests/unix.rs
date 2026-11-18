#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(unix)]
fn unix_mode_config(path: &std::path::Path, mode: Option<u16>) -> ConfigDraft {
    let mut config = empty_config();
    config.listeners.push(oxiroute_config::Listener {
        name: "unix".into(),
        bind: ListenerBind::Unix {
            path: path.to_owned(),
            mode,
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
    config
}

#[cfg(unix)]
fn socket_mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .expect("reserved socket metadata")
        .permissions()
        .mode()
        & 0o7777
}

#[cfg(unix)]
#[test]
fn a_changed_unix_mode_is_applied_to_the_kept_socket_and_keeps_its_connections() {
    use std::io::{Read as _, Write as _};

    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let path = directory.path().join("reload.sock");
    let default_path = directory.path().join("default.sock");
    let default = prepare_reservations(&unix_mode_config(&default_path, None), None)
        .expect("Unix bind without a mode");
    let first =
        prepare_reservations(&unix_mode_config(&path, Some(0o600)), None).expect("first Unix bind");
    assert_eq!(socket_mode(&path), 0o600);
    let listener = std::os::unix::net::UnixListener::from(
        first
            .get("unix")
            .expect("first")
            .duplicate_owned_fd()
            .expect("duplicate"),
    );
    let client = std::os::unix::net::UnixStream::connect(&path).expect("established client");
    let (mut accepted, _) = listener.accept().expect("accepted client");

    let loosened = prepare_reservations(&unix_mode_config(&path, Some(0o660)), Some(&first))
        .expect("mode change on the kept path");
    assert!(Arc::ptr_eq(
        &first.get("unix").expect("first").inner,
        &loosened.get("unix").expect("loosened").inner,
    ));
    assert_eq!(socket_mode(&path), 0o660);
    let mut tightened =
        prepare_reservations(&unix_mode_config(&path, Some(0o640)), Some(&loosened))
            .expect("tightening on the kept path");
    assert_eq!(socket_mode(&path), 0o640);
    drop((loosened, first));
    assert_eq!(socket_mode(&path), 0o640);

    (&client)
        .write_all(b"ping")
        .expect("established client survives");
    let mut received = [0_u8; 4];
    accepted
        .read_exact(&mut received)
        .expect("accepted side survives");
    assert_eq!(&received, b"ping");
    tightened = prepare_reservations(&unix_mode_config(&path, None), Some(&tightened))
        .expect("an unset mode returns to the bind default");
    assert_eq!(socket_mode(&path), socket_mode(&default_path));
    drop((tightened, default));
}

#[cfg(unix)]
#[test]
fn a_discarded_candidate_restores_the_unix_mode_it_changed() {
    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let path = directory.path().join("restore.sock");
    let active = prepare_reservations(&unix_mode_config(&path, Some(0o600)), None)
        .expect("active Unix bind");

    let candidate = prepare_reservations(&unix_mode_config(&path, Some(0o666)), Some(&active))
        .expect("candidate mode change");
    assert_eq!(socket_mode(&path), 0o666);
    drop(candidate);
    assert_eq!(socket_mode(&path), 0o600);

    let committed = prepare_reservations(&unix_mode_config(&path, Some(0o660)), Some(&active))
        .expect("committed mode change");
    let next = prepare_reservations(&unix_mode_config(&path, Some(0o660)), Some(&committed))
        .expect("next reload settles the committed mode");
    drop((committed, active));
    assert_eq!(socket_mode(&path), 0o660);
    drop(next);
}

#[cfg(unix)]
#[test]
fn validation_reuses_the_unix_path_without_changing_its_mode() {
    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let path = directory.path().join("validate.sock");
    let active = prepare_reservations(&unix_mode_config(&path, Some(0o600)), None)
        .expect("active Unix bind");
    let candidate = validated_test_config(&unix_mode_config(&path, Some(0o660)))
        .expect("valid candidate config");

    let validation = ListenerReservations::prepare_for_validation(&candidate, Some(&active))
        .expect("validation reuses the active Unix path");
    assert!(Arc::ptr_eq(
        &active.get("unix").expect("active").inner,
        &validation.get("unix").expect("validation").inner,
    ));
    assert_eq!(socket_mode(&path), 0o600);
    assert!(!ListenerReservations::listener_restart_required(
        crate::RuntimeMode::Direct,
        &validated_test_config(&unix_mode_config(&path, Some(0o600))).expect("valid active"),
        &candidate,
    ));
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
