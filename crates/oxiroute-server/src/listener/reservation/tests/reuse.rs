#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn matching_listener_reservations_are_reused_without_rebinding() {
    let address = crate::test_ports::unused_tcp_address();
    let first = prepare_reservations(&config("edge", address), None).expect("first reservation");
    let second =
        prepare_reservations(&config("edge", address), Some(&first)).expect("reused reservation");

    assert!(Arc::ptr_eq(
        &first.get("edge").expect("first").inner,
        &second.get("edge").expect("second").inner
    ));
}

#[cfg(unix)]
#[test]
fn udp_listener_reservation_owns_and_releases_the_datagram_socket() {
    let address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary UDP bind")
        .local_addr()
        .expect("UDP address");
    let bind = ListenerBind::Udp { address };
    let reservation =
        ListenerReservation::bind("datagram", &bind).expect("UDP listener reservation");
    let duplicate = reservation
        .duplicate_udp_socket()
        .expect("UDP descriptor duplicate");
    assert_eq!(duplicate.local_addr().expect("duplicate address"), address);
    assert_eq!(
        std::net::UdpSocket::bind(address)
            .expect_err("reserved UDP address must remain owned")
            .kind(),
        io::ErrorKind::AddrInUse
    );
    drop(duplicate);
    drop(reservation);
    std::net::UdpSocket::bind(address).expect("UDP address released");
}

#[test]
fn listener_reservations_are_reused_after_rename() {
    let address = crate::test_ports::unused_tcp_address();
    let first =
        prepare_reservations(&config("old-name", address), None).expect("first reservation");
    let second = prepare_reservations(&config("new-name", address), Some(&first))
        .expect("renamed reservation");

    assert!(Arc::ptr_eq(
        &first.get("old-name").expect("first").inner,
        &second.get("new-name").expect("second").inner
    ));
}

#[test]
fn statistics_reservations_are_reused_after_reorder() {
    let first_address = crate::test_ports::unused_tcp_address();
    let second_address = crate::test_ports::unused_tcp_address();
    let mut first_config = empty_config();
    first_config.stats = Some(oxiroute_config::Stats {
        binds: vec![first_address, second_address],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let first = prepare_reservations(&first_config, None).expect("first reservations");
    let mut second_config = first_config;
    second_config.stats.as_mut().expect("stats").binds.reverse();
    let second =
        prepare_reservations(&second_config, Some(&first)).expect("reordered reservations");

    assert!(Arc::ptr_eq(
        &first.stats(0).expect("first original").inner,
        &second.stats(1).expect("first reordered").inner,
    ));
    assert!(Arc::ptr_eq(
        &first.get("@stats-0").expect("legacy first original").inner,
        &first.stats(0).expect("typed first original").inner,
    ));
    assert!(Arc::ptr_eq(
        &first.stats(1).expect("second original").inner,
        &second.stats(0).expect("second reordered").inner,
    ));
}

#[test]
fn statistics_page_reservations_are_named_and_reused_by_bind() {
    let address = crate::test_ports::unused_tcp_address();
    let mut config = empty_config();
    config.stats = Some(oxiroute_config::Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![oxiroute_config::StatsPage {
            bind: address,
            uri_prefix: "/stats".into(),
            refresh_ms: 1_000,
            admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
    });

    let first = prepare_reservations(&config, None).expect("first reservations");
    let second = prepare_reservations(&config, Some(&first)).expect("reused page reservation");

    assert_eq!(first.len(), 1);
    assert!(Arc::ptr_eq(
        &first.stats_page(0).expect("first page").inner,
        &second.stats_page(0).expect("second page").inner,
    ));
    assert!(Arc::ptr_eq(
        &first.get("@stats-page-0").expect("legacy first page").inner,
        &first.stats_page(0).expect("typed first page").inner,
    ));
}

#[test]
fn traffic_name_cannot_collide_with_management_reservation() {
    let traffic = crate::test_ports::unused_tcp_address();
    let management = crate::test_ports::unused_tcp_address();
    let mut config = config("@management", traffic);
    config.management = Some(oxiroute_config::Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });

    let reservations = prepare_reservations(&config, None).expect("reservations");

    assert_eq!(reservations.len(), 2);
    assert_eq!(
        reservations
            .get("@management")
            .expect("traffic reservation")
            .bind_config(),
        &ListenerBind::Socket {
            address: traffic,
            ipv6_only: false
        }
    );
    assert_eq!(
        reservations
            .management()
            .expect("management reservation")
            .bind_config(),
        &ListenerBind::Socket {
            address: management,
            ipv6_only: false
        }
    );
}

#[cfg(unix)]
#[test]
fn wildcard_ipv6_listener_reservation_is_dual_stack() {
    use rustix::net::sockopt::ipv6_v6only;

    let probe = std::net::TcpListener::bind((std::net::Ipv6Addr::UNSPECIFIED, 0));
    let Ok(probe) = probe else {
        eprintln!("IPv6 is unavailable in this environment: dual-stack bind not exercised");
        return;
    };
    let address = probe.local_addr().expect("probe address");
    drop(probe);
    let reservation = ListenerReservation::bind(
        "dual",
        &ListenerBind::Socket {
            address,
            ipv6_only: false,
        },
    )
    .expect("wildcard IPv6 reservation");
    let listener =
        std::net::TcpListener::from(reservation.duplicate_owned_fd().expect("duplicate"));
    assert!(!ipv6_v6only(&listener).expect("IPV6_V6ONLY"));

    let port = address.port();
    for client in [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
    ] {
        std::net::TcpStream::connect(client).unwrap_or_else(|error| panic!("{client}: {error}"));
        listener.accept().expect("accepted client");
    }
}

#[cfg(unix)]
#[test]
fn ipv6_only_wildcard_shares_its_port_with_an_ipv4_wildcard() {
    use rustix::net::sockopt::ipv6_v6only;

    let probe = std::net::TcpListener::bind((std::net::Ipv6Addr::UNSPECIFIED, 0));
    let Ok(probe) = probe else {
        eprintln!("IPv6 is unavailable in this environment: IPv6-only bind not exercised");
        return;
    };
    let port = probe.local_addr().expect("probe address").port();
    drop(probe);
    let v4 = ListenerReservation::bind(
        "v4",
        &ListenerBind::Socket {
            address: SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
            ipv6_only: false,
        },
    )
    .expect("IPv4 wildcard reservation");
    let v6 = ListenerReservation::bind(
        "v6",
        &ListenerBind::Socket {
            address: SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
            ipv6_only: true,
        },
    )
    .expect("IPv6-only wildcard reservation beside the IPv4 one");
    let listen = |reservation: &ListenerReservation| {
        std::net::TcpListener::from(reservation.duplicate_owned_fd().expect("duplicate"))
    };
    let (v4, v6) = (listen(&v4), listen(&v6));
    assert!(ipv6_v6only(&v6).expect("IPV6_V6ONLY"));

    for (client, listener) in [
        (SocketAddr::from((Ipv4Addr::LOCALHOST, port)), &v4),
        (SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)), &v6),
    ] {
        std::net::TcpStream::connect(client).unwrap_or_else(|error| panic!("{client}: {error}"));
        listener
            .accept()
            .expect("accepted by the socket of its family");
    }
}
