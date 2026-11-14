#[allow(clippy::wildcard_imports)]
use super::*;
use crate::test_ports::unused_tcp_address;

#[test]
fn validation_tcp_probe_holds_then_releases_the_ephemeral_address() {
    let address = unused_tcp_address();
    let mut config = many_listener_config(1).to_draft();
    config.listeners[0].bind = ListenerBind::Socket {
        address,
        ipv6_only: false,
    };
    let config = config.validate().expect("valid TCP probe config");

    let reservations = ListenerReservations::prepare_for_validation(&config, None)
        .expect("validation TCP reservation");
    assert!(
        std::net::TcpListener::bind(address).is_err(),
        "validation did not hold the TCP probe"
    );
    drop(reservations);
    std::net::TcpListener::bind(address).expect("validation released the TCP probe");
}

#[cfg(unix)]
#[test]
fn validation_unix_probe_removes_its_temporary_namespace_entries() {
    let directory = tempfile::tempdir().expect("Unix validation directory");
    let path = directory.path().join("validation.sock");
    let mut config = many_listener_config(1).to_draft();
    config.listeners[0].bind = ListenerBind::Unix {
        path: path.clone(),
        mode: Some(0o600),
    };
    let config = config.validate().expect("valid Unix probe config");

    let reservations = ListenerReservations::prepare_for_validation(&config, None)
        .expect("validation Unix reservation");
    assert!(path.exists(), "validation did not create the Unix probe");
    drop(reservations);
    assert!(!path.exists(), "validation retained the Unix probe");
    assert!(
        !unix_socket_marker_path(&path)
            .expect("Unix marker path")
            .exists(),
        "validation retained the Unix lease marker"
    );
}

#[cfg(unix)]
#[test]
fn validation_source_preserves_direct_capacity_and_unix_path_only_reuse_semantics() {
    let direct = many_listener_config(MAX_DESCRIPTOR_COUNT + 1);
    assert_eq!(
        ListenerReservations::prepare_for_validation(&direct, None)
            .expect("direct validation accepts 65 listeners")
            .len(),
        65
    );

    let directory = tempfile::tempdir().expect("validation source directory");
    let path = directory.path().join("validation-source.sock");
    let mut active_config = empty_config();
    active_config.listeners.push(oxiroute_config::Listener {
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
    let active = prepare_reservations(&active_config, None).expect("active Unix reservation");
    let mut changed = active_config.clone();
    changed.listeners[0].bind = ListenerBind::Unix {
        path: path.clone(),
        mode: Some(0o660),
    };
    let changed = validated_test_config(&changed).expect("changed Unix validation config");

    let validation = ListenerReservations::prepare_for_validation(&changed, Some(&active))
        .expect("validation reuses active Unix path");
    assert!(Arc::ptr_eq(
        &active.get("unix").expect("active Unix reservation").inner,
        &validation
            .get("unix")
            .expect("validation Unix reservation")
            .inner,
    ));
    assert!(ListenerReservations::prepare(&changed, Some(&active)).is_err());
    drop(validation);
    assert!(path.exists(), "validation unlinked the active Unix socket");
    drop(active);
    assert!(!path.exists(), "active owner retained the Unix socket");
}
