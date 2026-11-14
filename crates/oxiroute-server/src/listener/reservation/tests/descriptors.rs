#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(target_os = "linux")]
#[test]
fn descriptor_export_is_canonical_typed_and_cloexec() {
    use rustix::io::{FdFlags, fcntl_getfd};

    let traffic = crate::test_ports::unused_tcp_address();
    let management = crate::test_ports::unused_tcp_address();
    let stats = crate::test_ports::unused_tcp_address();
    let page = crate::test_ports::unused_tcp_address();
    let mut config = config("@management", traffic);
    config.management = Some(oxiroute_config::Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });
    config.stats = Some(oxiroute_config::Stats {
        binds: vec![stats],
        admin_token_file: None,
        pages: vec![oxiroute_config::StatsPage {
            bind: page,
            uri_prefix: "/stats".into(),
            refresh_ms: 1_000,
            admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
    });
    let reservations = prepare_reservations(&config, None).expect("reservations");

    let (manifest, originals) =
        export_reservations(&reservations, &config).expect("descriptor export");

    assert_eq!(originals.len(), 4);
    assert_eq!(
        manifest
            .slots()
            .iter()
            .map(|slot| (slot.id, slot.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            (SlotId(0), DescriptorRole::Traffic("@management".into())),
            (SlotId(1), DescriptorRole::Management),
            (SlotId(2), DescriptorRole::Stats(0)),
            (SlotId(3), DescriptorRole::StatsPage(0)),
        ]
    );
    assert!(originals.iter().all(|descriptor| {
        fcntl_getfd(descriptor)
            .expect("descriptor flags")
            .contains(FdFlags::CLOEXEC)
    }));
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_export_and_adoption_supports_udp_relay_listeners() {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("temporary UDP bind");
    socket
        .set_nonblocking(true)
        .expect("nonblocking UDP socket");
    let address = socket.local_addr().expect("UDP address");
    drop(socket);
    let mut config = config("udp", address);
    config.listeners[0].bind = ListenerBind::Udp { address };
    config.listeners[0].protocol = oxiroute_config::Protocol::Udp;
    let reservations = prepare_reservations(&config, None).expect("reservations");
    let (manifest, originals) =
        export_reservations(&reservations, &config).expect("UDP descriptor export");

    assert_eq!(manifest.slots()[0].kind, DescriptorKind::DatagramListener);
    assert_eq!(manifest.slots()[0].bind, Some(BindIdentity::Tcp(address)));
    let descriptors = DescriptorSet::new(&manifest, originals).expect("UDP descriptor set");
    let adopted = adopt_reservations(&config, descriptors).expect("UDP adoption");
    let duplicate = adopted
        .get("udp")
        .expect("adopted UDP reservation")
        .duplicate_udp_socket()
        .expect("adopted UDP duplicate");
    assert_eq!(duplicate.local_addr().expect("duplicate address"), address);
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_export_and_adoption_supports_quic_listeners() {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("temporary QUIC bind");
    socket
        .set_nonblocking(true)
        .expect("nonblocking QUIC socket");
    let address = socket.local_addr().expect("QUIC address");
    drop(socket);
    let mut config = config("h3", address);
    config.listeners[0].bind = ListenerBind::Udp { address };
    config.listeners[0].protocol = oxiroute_config::Protocol::Http3;
    let reservations = prepare_reservations(&config, None).expect("reservations");
    let (manifest, originals) =
        export_reservations(&reservations, &config).expect("QUIC descriptor export");

    assert_eq!(manifest.slots()[0].kind, DescriptorKind::QuicListener);
    assert_eq!(manifest.slots()[0].bind, Some(BindIdentity::Tcp(address)));
    let descriptors = DescriptorSet::new(&manifest, originals).expect("QUIC descriptor set");
    let adopted = adopt_reservations(&config, descriptors).expect("QUIC adoption");
    let duplicate = adopted
        .get("h3")
        .expect("adopted QUIC reservation")
        .duplicate_udp_socket()
        .expect("adopted QUIC duplicate");
    assert_eq!(duplicate.local_addr().expect("duplicate address"), address);
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_adoption_rejects_a_wrong_typed_slot() {
    let address = crate::test_ports::unused_tcp_address();
    let config = config("traffic", address);
    let reservations = prepare_reservations(&config, None).expect("reservations");
    let (manifest, originals) =
        export_reservations(&reservations, &config).expect("descriptor export");
    let mut wrong_slots = manifest.slots().to_vec();
    wrong_slots[0].role = DescriptorRole::Management;
    let wrong = DescriptorManifest::new(wrong_slots).expect("wrong typed manifest");
    let set = DescriptorSet::new(&wrong, originals).expect("kernel-valid descriptor set");

    let error = adopt_reservations(&config, set)
        .err()
        .expect("typed slot mismatch");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_export_rejects_extra_keys_and_bind_mismatch() {
    let address = crate::test_ports::unused_tcp_address();
    let config = config("traffic", address);
    let mut reservations = prepare_reservations(&config, None).expect("reservations");
    let traffic = reservations.get("traffic").expect("traffic").clone();
    reservations
        .by_key
        .insert(ReservationKey::Management, traffic);
    let error = export_reservations(&reservations, &config).expect_err("extra key must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    drop(reservations);

    let reservations = prepare_reservations(&config, None).expect("reservations");
    let mut changed = config;
    changed.listeners[0].bind = ListenerBind::Socket {
        address: "127.0.0.1:1".parse().expect("changed address"),
        ipv6_only: false,
    };
    let error = export_reservations(&reservations, &changed).expect_err("bind mismatch must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[cfg(target_os = "linux")]
#[test]
fn worker_descriptor_limit_is_explicitly_sixty_four() {
    let mut config = empty_config();
    config.listeners = (0..=MAX_DESCRIPTOR_COUNT)
        .map(|index| oxiroute_config::Listener {
            name: format!("listener-{index}"),
            bind: ListenerBind::Socket {
                address: SocketAddr::from((
                    [127, 0, 0, 1],
                    10_000 + u16::try_from(index).expect("bounded listener index"),
                )),
                ipv6_only: false,
            },
            protocol: oxiroute_config::Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        })
        .collect();

    let config = validated_test_config(&config).expect("valid oversized config");
    let error = descriptor_slots(&ListenerInventory::compile(&config))
        .expect_err("65 descriptors must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("maximum is 64"));
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_capacity_calculation_includes_amplification_and_headroom() {
    let mut config = config("tcp", "127.0.0.1:8080".parse().expect("address"));
    config.listeners.push(oxiroute_config::Listener {
        name: "unix".into(),
        bind: ListenerBind::Unix {
            path: PathBuf::from("/tmp/capacity.sock"),
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
    let config = validated_test_config(&config).expect("valid capacity config");
    let inventory = ListenerInventory::compile(&config);

    assert_eq!(descriptor_capacity_required(&inventory, 11), 50);
    assert!(validate_descriptor_capacity(&inventory, 11, Some(50)).is_ok());
    assert!(validate_descriptor_capacity(&inventory, 11, None).is_ok());
    let error = validate_descriptor_capacity(&inventory, 11, Some(49))
        .expect_err("low descriptor limit must fail");
    assert!(error.to_string().contains("at least 50"));
    assert!(error.to_string().contains("11 currently open"));
    assert!(error.to_string().contains("soft limit is 49"));
}

#[cfg(target_os = "linux")]
#[test]
fn adopted_unix_reservation_never_owns_master_namespace_cleanup() {
    let directory = tempfile::tempdir().expect("Unix reservation directory");
    let path = directory.path().join("adopted.sock");
    let mut config = empty_config();
    config.listeners.push(oxiroute_config::Listener {
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
    let master = prepare_reservations(&config, None).expect("master reservation");
    let (manifest, originals) = export_reservations(&master, &config).expect("descriptor export");
    let set = DescriptorSet::new(&manifest, originals).expect("descriptor set");
    let worker = adopt_reservations(&config, set).expect("worker adoption");
    let marker = unix_socket_marker_path(&path).expect("marker path");

    drop(worker);
    assert!(path.exists(), "worker drop unlinked the master socket");
    assert!(marker.exists(), "worker drop unlinked the master lease");

    drop(master);
    assert!(!path.exists(), "master drop retained the socket");
    assert!(!marker.exists(), "master drop retained the lease marker");
}
