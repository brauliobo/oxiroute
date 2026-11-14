#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq)]
struct ExpectedInventoryEntry {
    id: ListenerId,
    name: &'static str,
    protocol: Protocol,
    bind: ListenerBind,
    descriptor_role: ListenerDescriptorRole,
    descriptor_kind: ListenerDescriptorKind,
    descriptor_protocol: Option<Protocol>,
    policy: (ListenerPlane, ListenerMetricPolicy, Option<u64>),
}

#[cfg(target_os = "linux")]
fn assert_inventory_and_slots(
    config: &ValidatedConfig,
    expected_inventory: &[ExpectedInventoryEntry],
    expected_slots: &[DescriptorSlot],
) {
    let inventory = ListenerInventory::compile(config);
    let slots = descriptor_slots(&inventory).expect("descriptor slots");
    assert_eq!(slots, expected_slots);
    assert_eq!(inventory.entries().len(), expected_inventory.len());
    for (actual, expected) in inventory.entries().iter().zip(expected_inventory) {
        assert_eq!(actual.id, expected.id);
        assert_eq!(actual.name, expected.name);
        assert_eq!(actual.protocol, expected.protocol);
        assert_eq!(actual.bind, expected.bind);
        assert_eq!(actual.descriptor_role, expected.descriptor_role);
        assert_eq!(actual.descriptor_kind, expected.descriptor_kind);
        assert_eq!(actual.descriptor_protocol(), expected.descriptor_protocol);
        assert_eq!(
            (actual.plane, actual.metric_policy, actual.max_connections),
            expected.policy
        );
    }
}

#[cfg(target_os = "linux")]
fn expected_traffic(
    name: &'static str,
    port: u16,
    protocol: Protocol,
    descriptor_kind: ListenerDescriptorKind,
    max_connections: Option<u64>,
) -> ExpectedInventoryEntry {
    ExpectedInventoryEntry {
        id: ListenerId::Traffic(name.into()),
        name,
        protocol,
        bind: ListenerBind::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            ipv6_only: false,
        },
        descriptor_role: ListenerDescriptorRole::Traffic(name.into()),
        descriptor_kind,
        descriptor_protocol: Some(protocol),
        policy: (
            ListenerPlane::Data,
            ListenerMetricPolicy::Public,
            max_connections,
        ),
    }
}

#[cfg(target_os = "linux")]
fn expected_udp_traffic(
    name: &'static str,
    port: u16,
    protocol: Protocol,
    descriptor_kind: ListenerDescriptorKind,
) -> ExpectedInventoryEntry {
    ExpectedInventoryEntry {
        id: ListenerId::Traffic(name.into()),
        name,
        protocol,
        bind: ListenerBind::Udp {
            address: SocketAddr::from(([127, 0, 0, 1], port)),
        },
        descriptor_role: ListenerDescriptorRole::Traffic(name.into()),
        descriptor_kind,
        descriptor_protocol: Some(protocol),
        policy: (ListenerPlane::Data, ListenerMetricPolicy::Public, None),
    }
}

#[cfg(target_os = "linux")]
fn expected_stats(index: usize, port: u16) -> ExpectedInventoryEntry {
    ExpectedInventoryEntry {
        id: ListenerId::Stats(index),
        name: if index == 0 { "@stats-0" } else { "@stats-1" },
        protocol: Protocol::Http,
        bind: ListenerBind::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            ipv6_only: false,
        },
        descriptor_role: ListenerDescriptorRole::Stats(index),
        descriptor_kind: ListenerDescriptorKind::Tcp,
        descriptor_protocol: None,
        policy: (
            ListenerPlane::Control,
            ListenerMetricPolicy::InternalOnly,
            None,
        ),
    }
}

#[cfg(target_os = "linux")]
fn expected_tcp_slot(id: u16, role: DescriptorRole, port: u16) -> DescriptorSlot {
    DescriptorSlot {
        id: SlotId(id),
        role,
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp(SocketAddr::from(([127, 0, 0, 1], port)))),
        mode: None,
    }
}

#[cfg(target_os = "linux")]
fn expected_datagram_slot(id: u16, name: &str, port: u16, kind: DescriptorKind) -> DescriptorSlot {
    DescriptorSlot {
        id: SlotId(id),
        role: DescriptorRole::Traffic(name.into()),
        kind,
        bind: Some(BindIdentity::Tcp(SocketAddr::from(([127, 0, 0, 1], port)))),
        mode: None,
    }
}

#[cfg(target_os = "linux")]
fn historical_descriptor_plan(
    config: &ValidatedConfig,
) -> io::Result<Vec<(ReservationKey, DescriptorSlot)>> {
    let config = config.as_draft();
    let count = config.listeners.len()
        + usize::from(config.management.is_some())
        + config
            .stats
            .as_ref()
            .map_or(0, |stats| stats.binds.len() + stats.pages.len());
    if count > MAX_DESCRIPTOR_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worker configuration has {count} listener descriptors; maximum is {MAX_DESCRIPTOR_COUNT}"
            ),
        ));
    }

    let mut entries = Vec::with_capacity(count);
    for listener in &config.listeners {
        entries.push((
            ListenerId::Traffic(listener.name.clone()),
            DescriptorRole::Traffic(listener.name.clone()),
            Some(listener.protocol),
            listener.bind.clone(),
        ));
    }
    if let Some(management) = &config.management {
        entries.push((
            ListenerId::Management,
            DescriptorRole::Management,
            None,
            ListenerBind::Socket {
                address: management.bind,
                ipv6_only: false,
            },
        ));
    }
    if let Some(stats) = &config.stats {
        for (index, address) in stats.binds.iter().enumerate() {
            entries.push((
                ListenerId::Stats(index),
                DescriptorRole::Stats(u16::try_from(index).expect("descriptor limit checked")),
                None,
                ListenerBind::Socket {
                    address: *address,
                    ipv6_only: false,
                },
            ));
        }
        for (index, page) in stats.pages.iter().enumerate() {
            entries.push((
                ListenerId::StatsPage(index),
                DescriptorRole::StatsPage(u16::try_from(index).expect("descriptor limit checked")),
                None,
                ListenerBind::Socket {
                    address: page.bind,
                    ipv6_only: false,
                },
            ));
        }
    }

    entries
        .into_iter()
        .enumerate()
        .map(|(index, (key, role, protocol, bind))| {
            Ok((
                key,
                historical_descriptor_slot(
                    SlotId(u16::try_from(index).expect("descriptor limit checked")),
                    role,
                    protocol,
                    &bind,
                )?,
            ))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn historical_descriptor_slot(
    id: SlotId,
    role: DescriptorRole,
    protocol: Option<Protocol>,
    bind: &ListenerBind,
) -> io::Result<DescriptorSlot> {
    let (kind, bind, mode) = match bind {
        ListenerBind::Socket { address, .. } => (
            DescriptorKind::TcpListener,
            Some(BindIdentity::Tcp(*address)),
            None,
        ),
        ListenerBind::Unix { path, mode } => (
            DescriptorKind::UnixListener,
            Some(BindIdentity::UnixPath(path.clone())),
            *mode,
        ),
        ListenerBind::Udp { address } => {
            let kind = match protocol {
                Some(Protocol::Udp) => DescriptorKind::DatagramListener,
                Some(Protocol::ForwardHttp3 | Protocol::Http3) => DescriptorKind::QuicListener,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("UDP listener `{address}` has an unsupported protocol"),
                    ));
                }
            };
            (kind, Some(BindIdentity::Tcp(*address)), None)
        }
    };
    Ok(DescriptorSlot {
        id,
        role,
        kind,
        bind,
        mode,
    })
}

#[cfg(target_os = "linux")]
fn assert_historical_descriptor_plan_parity(config: &ValidatedConfig) {
    let expected = historical_descriptor_plan(config).expect("historical descriptor plan");
    let inventory = ListenerInventory::compile(config);
    let slots = descriptor_slots(&inventory).expect("descriptor slots");
    let actual = inventory
        .entries()
        .iter()
        .map(|entry| entry.id.clone())
        .zip(slots)
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[cfg(target_os = "linux")]
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential assertion pins every inventory field in canonical order"
)]
fn inventory_and_descriptor_slots_match_independent_mixed_expectations() {
    let config = differential_config();
    let expected_inventory = vec![
        expected_traffic(
            "http",
            7_996,
            Protocol::Http,
            ListenerDescriptorKind::Tcp,
            Some(7),
        ),
        expected_traffic(
            "rtmp",
            7_997,
            Protocol::Rtmp,
            ListenerDescriptorKind::Tcp,
            Some(8),
        ),
        expected_traffic(
            "forward-h1",
            7_998,
            Protocol::ForwardHttp1,
            ListenerDescriptorKind::Tcp,
            Some(9),
        ),
        expected_traffic(
            "forward-h2",
            7_999,
            Protocol::ForwardHttp2,
            ListenerDescriptorKind::Tcp,
            Some(10),
        ),
        expected_traffic(
            "tcp",
            8_000,
            Protocol::Tcp,
            ListenerDescriptorKind::Tcp,
            Some(11),
        ),
        ExpectedInventoryEntry {
            id: ListenerId::Traffic("unix".into()),
            name: "unix",
            protocol: Protocol::Tcp,
            bind: ListenerBind::Unix {
                path: PathBuf::from("/tmp/inventory.sock"),
                mode: Some(0o600),
            },
            descriptor_role: ListenerDescriptorRole::Traffic("unix".into()),
            descriptor_kind: ListenerDescriptorKind::Unix,
            descriptor_protocol: Some(Protocol::Tcp),
            policy: (ListenerPlane::Data, ListenerMetricPolicy::Public, None),
        },
        expected_udp_traffic(
            "udp",
            8_001,
            Protocol::Udp,
            ListenerDescriptorKind::Datagram,
        ),
        expected_udp_traffic("h3", 8_002, Protocol::Http3, ListenerDescriptorKind::Quic),
        expected_udp_traffic(
            "forward-h3",
            8_003,
            Protocol::ForwardHttp3,
            ListenerDescriptorKind::Quic,
        ),
        ExpectedInventoryEntry {
            id: ListenerId::Management,
            name: "@management",
            protocol: Protocol::Http,
            bind: ListenerBind::Socket {
                address: "127.0.0.1:9900".parse().expect("management bind"),
                ipv6_only: false,
            },
            descriptor_role: ListenerDescriptorRole::Management,
            descriptor_kind: ListenerDescriptorKind::Tcp,
            descriptor_protocol: None,
            policy: (
                ListenerPlane::Control,
                ListenerMetricPolicy::InternalOnly,
                None,
            ),
        },
        expected_stats(0, 8_404),
        expected_stats(1, 8_405),
        ExpectedInventoryEntry {
            id: ListenerId::StatsPage(0),
            name: "@stats-page-0",
            protocol: Protocol::Http,
            bind: ListenerBind::Socket {
                address: "127.0.0.1:8406".parse().expect("stats page bind"),
                ipv6_only: false,
            },
            descriptor_role: ListenerDescriptorRole::StatsPage(0),
            descriptor_kind: ListenerDescriptorKind::Tcp,
            descriptor_protocol: None,
            policy: (
                ListenerPlane::Control,
                ListenerMetricPolicy::Public,
                Some(17),
            ),
        },
    ];
    let expected_slots = vec![
        expected_tcp_slot(0, DescriptorRole::Traffic("http".into()), 7_996),
        expected_tcp_slot(1, DescriptorRole::Traffic("rtmp".into()), 7_997),
        expected_tcp_slot(2, DescriptorRole::Traffic("forward-h1".into()), 7_998),
        expected_tcp_slot(3, DescriptorRole::Traffic("forward-h2".into()), 7_999),
        expected_tcp_slot(4, DescriptorRole::Traffic("tcp".into()), 8_000),
        DescriptorSlot {
            id: SlotId(5),
            role: DescriptorRole::Traffic("unix".into()),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(PathBuf::from("/tmp/inventory.sock"))),
            mode: Some(0o600),
        },
        expected_datagram_slot(6, "udp", 8_001, DescriptorKind::DatagramListener),
        expected_datagram_slot(7, "h3", 8_002, DescriptorKind::QuicListener),
        expected_datagram_slot(8, "forward-h3", 8_003, DescriptorKind::QuicListener),
        expected_tcp_slot(9, DescriptorRole::Management, 9_900),
        expected_tcp_slot(10, DescriptorRole::Stats(0), 8_404),
        expected_tcp_slot(11, DescriptorRole::Stats(1), 8_405),
        expected_tcp_slot(12, DescriptorRole::StatsPage(0), 8_406),
    ];
    assert_inventory_and_slots(&config, &expected_inventory, &expected_slots);
    assert_historical_descriptor_plan_parity(&config);

    let mut changed = config.to_draft();
    changed.listeners.swap(0, 8);
    changed.listeners[0].name = "renamed-forward-h3".into();
    let changed = changed
        .validate()
        .expect("valid reordered and renamed config");
    let renamed_reordered_inventory = vec![
        expected_udp_traffic(
            "renamed-forward-h3",
            8_003,
            Protocol::ForwardHttp3,
            ListenerDescriptorKind::Quic,
        ),
        expected_traffic(
            "rtmp",
            7_997,
            Protocol::Rtmp,
            ListenerDescriptorKind::Tcp,
            Some(8),
        ),
        expected_traffic(
            "forward-h1",
            7_998,
            Protocol::ForwardHttp1,
            ListenerDescriptorKind::Tcp,
            Some(9),
        ),
        expected_traffic(
            "forward-h2",
            7_999,
            Protocol::ForwardHttp2,
            ListenerDescriptorKind::Tcp,
            Some(10),
        ),
        expected_traffic(
            "tcp",
            8_000,
            Protocol::Tcp,
            ListenerDescriptorKind::Tcp,
            Some(11),
        ),
        ExpectedInventoryEntry {
            id: ListenerId::Traffic("unix".into()),
            name: "unix",
            protocol: Protocol::Tcp,
            bind: ListenerBind::Unix {
                path: PathBuf::from("/tmp/inventory.sock"),
                mode: Some(0o600),
            },
            descriptor_role: ListenerDescriptorRole::Traffic("unix".into()),
            descriptor_kind: ListenerDescriptorKind::Unix,
            descriptor_protocol: Some(Protocol::Tcp),
            policy: (ListenerPlane::Data, ListenerMetricPolicy::Public, None),
        },
        expected_udp_traffic(
            "udp",
            8_001,
            Protocol::Udp,
            ListenerDescriptorKind::Datagram,
        ),
        expected_udp_traffic("h3", 8_002, Protocol::Http3, ListenerDescriptorKind::Quic),
        expected_traffic(
            "http",
            7_996,
            Protocol::Http,
            ListenerDescriptorKind::Tcp,
            Some(7),
        ),
        ExpectedInventoryEntry {
            id: ListenerId::Management,
            name: "@management",
            protocol: Protocol::Http,
            bind: ListenerBind::Socket {
                address: "127.0.0.1:9900".parse().expect("management bind"),
                ipv6_only: false,
            },
            descriptor_role: ListenerDescriptorRole::Management,
            descriptor_kind: ListenerDescriptorKind::Tcp,
            descriptor_protocol: None,
            policy: (
                ListenerPlane::Control,
                ListenerMetricPolicy::InternalOnly,
                None,
            ),
        },
        expected_stats(0, 8_404),
        expected_stats(1, 8_405),
        ExpectedInventoryEntry {
            id: ListenerId::StatsPage(0),
            name: "@stats-page-0",
            protocol: Protocol::Http,
            bind: ListenerBind::Socket {
                address: "127.0.0.1:8406".parse().expect("stats page bind"),
                ipv6_only: false,
            },
            descriptor_role: ListenerDescriptorRole::StatsPage(0),
            descriptor_kind: ListenerDescriptorKind::Tcp,
            descriptor_protocol: None,
            policy: (
                ListenerPlane::Control,
                ListenerMetricPolicy::Public,
                Some(17),
            ),
        },
    ];
    let renamed_reordered_slots = vec![
        expected_datagram_slot(0, "renamed-forward-h3", 8_003, DescriptorKind::QuicListener),
        expected_tcp_slot(1, DescriptorRole::Traffic("rtmp".into()), 7_997),
        expected_tcp_slot(2, DescriptorRole::Traffic("forward-h1".into()), 7_998),
        expected_tcp_slot(3, DescriptorRole::Traffic("forward-h2".into()), 7_999),
        expected_tcp_slot(4, DescriptorRole::Traffic("tcp".into()), 8_000),
        DescriptorSlot {
            id: SlotId(5),
            role: DescriptorRole::Traffic("unix".into()),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(PathBuf::from("/tmp/inventory.sock"))),
            mode: Some(0o600),
        },
        expected_datagram_slot(6, "udp", 8_001, DescriptorKind::DatagramListener),
        expected_datagram_slot(7, "h3", 8_002, DescriptorKind::QuicListener),
        expected_tcp_slot(8, DescriptorRole::Traffic("http".into()), 7_996),
        expected_tcp_slot(9, DescriptorRole::Management, 9_900),
        expected_tcp_slot(10, DescriptorRole::Stats(0), 8_404),
        expected_tcp_slot(11, DescriptorRole::Stats(1), 8_405),
        expected_tcp_slot(12, DescriptorRole::StatsPage(0), 8_406),
    ];
    assert_inventory_and_slots(
        &changed,
        &renamed_reordered_inventory,
        &renamed_reordered_slots,
    );
    assert_historical_descriptor_plan_parity(&changed);

    let mut policy_only = config.to_draft();
    policy_only.listeners[0].max_connections = Some(70);
    let policy_only = policy_only.validate().expect("valid policy-only change");
    assert_historical_descriptor_plan_parity(&policy_only);
}

#[cfg(target_os = "linux")]
#[test]
fn inventory_has_no_supervised_descriptor_limit() {
    let sixty_four = many_listener_config(MAX_DESCRIPTOR_COUNT);
    assert_eq!(ListenerInventory::compile(&sixty_four).entries().len(), 64);
    assert_eq!(
        descriptor_slots(&ListenerInventory::compile(&sixty_four))
            .expect("64 descriptors are accepted")
            .len(),
        64
    );
    assert_eq!(
        historical_descriptor_plan(&sixty_four)
            .expect("historical plan accepts 64 descriptors")
            .len(),
        64
    );

    let sixty_five = many_listener_config(MAX_DESCRIPTOR_COUNT + 1);
    assert_eq!(ListenerInventory::compile(&sixty_five).entries().len(), 65);
    let error = descriptor_slots(&ListenerInventory::compile(&sixty_five))
        .expect_err("existing supervised oracle rejects 65 descriptors");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("maximum is 64"));
    let historical_error = historical_descriptor_plan(&sixty_five)
        .expect_err("historical descriptor plan rejects 65 descriptors");
    assert_eq!(historical_error.kind(), io::ErrorKind::InvalidInput);
    assert!(historical_error.to_string().contains("maximum is 64"));
}
