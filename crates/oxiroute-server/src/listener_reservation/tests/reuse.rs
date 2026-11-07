#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn matching_listener_reservations_are_reused_without_rebinding() {
    let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary bind")
        .local_addr()
        .expect("address");
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
    let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary bind")
        .local_addr()
        .expect("address");
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
    let first_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("first temporary bind")
        .local_addr()
        .expect("first address");
    let second_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("second temporary bind")
        .local_addr()
        .expect("second address");
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
    let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary bind")
        .local_addr()
        .expect("address");
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
    let traffic = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("traffic temporary bind")
        .local_addr()
        .expect("traffic address");
    let management = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("management temporary bind")
        .local_addr()
        .expect("management address");
    let mut config = config("@management", traffic);
    config.management = Some(oxiroute_config::Management {
        bind: management,
        ui_dir: None,
    });

    let reservations = prepare_reservations(&config, None).expect("reservations");

    assert_eq!(reservations.len(), 2);
    assert_eq!(
        reservations
            .get("@management")
            .expect("traffic reservation")
            .bind_config(),
        &ListenerBind::Socket { address: traffic }
    );
    assert_eq!(
        reservations
            .management()
            .expect("management reservation")
            .bind_config(),
        &ListenerBind::Socket {
            address: management
        }
    );
}
