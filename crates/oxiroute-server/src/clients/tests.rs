use std::{net::SocketAddr, sync::Arc, time::Instant};

use super::*;

fn registry(max_clients: u64, ipv6_prefix: u8) -> Arc<Registry> {
    Arc::new(Registry::new(Settings {
        enabled: true,
        max_clients,
        ipv6_prefix,
    }))
}

fn open(registry: &Arc<Registry>, peer: &str, listener: &str, protocol: Protocol) -> Connection {
    registry
        .open(Arrival {
            peer: peer.parse::<SocketAddr>().expect("a socket address"),
            listener: Arc::from(listener),
            protocol,
            now: Instant::now(),
        })
        .expect("tracking is enabled")
}

fn clients(registry: &Registry, limit: usize, sort: Sort) -> ClientsView {
    registry.clients(
        &ClientQuery {
            limit,
            sort,
            ip_prefix: None,
        },
        Instant::now(),
    )
}

fn connections(registry: &Registry, limit: usize) -> ConnectionsView {
    registry.connections(
        &ConnectionQuery {
            limit,
            ip_prefix: None,
            listener: None,
        },
        Instant::now(),
    )
}

#[test]
fn a_live_connection_is_a_client_row() {
    let registry = registry(100, 128);
    let connection = open(&registry, "203.0.113.7:4000", "edge", Protocol::Http1);
    connection.stats().received::<Exclusive>(10, Instant::now());
    connection.stats().sent::<Exclusive>(90, Instant::now());
    connection
        .stats()
        .completed::<Exclusive>(false, Instant::now());
    let view = clients(&registry, 10, Sort::Bytes);
    assert_eq!(view.tracked_clients, 1);
    let row = &view.clients[0];
    assert_eq!(row.ip.to_string(), "203.0.113.7");
    assert_eq!(
        (row.connections, row.connections_total, row.requests),
        (1, 1, 1)
    );
    assert_eq!((row.bytes_in, row.bytes_out, row.errors), (10, 90, 0));
    assert_eq!(row.listeners[0].as_ref(), "edge");
    assert_eq!(row.protocols, ["http/1.1"]);
    assert_eq!(view.totals.bytes_out, 90);
    assert_eq!(view.other, Bucket::default());
}

#[test]
fn a_closed_connection_folds_into_its_client_and_counters_never_decrease() {
    let registry = registry(100, 128);
    let first = open(&registry, "203.0.113.7:4000", "edge", Protocol::Http1);
    first.stats().received::<Exclusive>(100, Instant::now());
    first.stats().completed::<Exclusive>(true, Instant::now());
    let before = clients(&registry, 10, Sort::Bytes).clients[0].clone();
    drop(first);
    let after = clients(&registry, 10, Sort::Bytes).clients[0].clone();
    assert_eq!((after.connections, after.connections_total), (0, 1));
    assert_eq!(
        (after.requests, after.bytes_in, after.errors),
        (before.requests, before.bytes_in, before.errors)
    );
    let second = open(&registry, "203.0.113.7:4001", "edge", Protocol::H2);
    second.stats().received::<Exclusive>(1, Instant::now());
    let later = clients(&registry, 10, Sort::Bytes).clients[0].clone();
    assert_eq!((later.connections, later.connections_total), (1, 2));
    assert_eq!(later.bytes_in, 101);
    assert_eq!(later.protocols, ["http/1.1", "h2"]);
    assert!(later.first_seen_age >= later.last_activity_age);
}

#[test]
fn concurrent_writers_lose_no_bytes() {
    let registry = registry(100, 128);
    let connection = open(&registry, "203.0.113.7:4000", "edge", Protocol::H2);
    let stats = Arc::clone(connection.stats());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..1000 {
                    stats.sent::<Concurrent>(3, Instant::now());
                }
            });
        }
    });
    assert_eq!(
        clients(&registry, 1, Sort::Bytes).clients[0].bytes_out,
        24_000
    );
}

#[test]
fn ipv6_peers_of_one_prefix_are_one_client_at_64_and_many_at_128() {
    let narrow = registry(100, 128);
    let wide = registry(100, 64);
    let held = [&narrow, &wide].map(|registry| {
        [
            open(registry, "[2001:db8:1:2::1]:80", "edge", Protocol::Http1),
            open(registry, "[2001:db8:1:2::2]:80", "edge", Protocol::Http1),
            open(registry, "[2001:db8:1:3::1]:80", "edge", Protocol::Http1),
        ]
    });
    assert_eq!(clients(&narrow, 10, Sort::Bytes).tracked_clients, 3);
    let view = clients(&wide, 10, Sort::Connections);
    assert_eq!(view.tracked_clients, 2);
    assert_eq!(view.clients[0].ip.to_string(), "2001:db8:1:2::");
    assert_eq!(view.clients[0].connections, 2);
    drop(held);
}

#[test]
fn the_limit_moves_the_rest_into_other_and_totals_cover_everything() {
    let registry = registry(100, 128);
    let mut held = Vec::new();
    for (index, bytes) in [5_u64, 50, 500].into_iter().enumerate() {
        let connection = open(
            &registry,
            &format!("198.51.100.{}:9", index + 1),
            "edge",
            Protocol::Tcp,
        );
        connection.stats().sent::<Exclusive>(bytes, Instant::now());
        held.push(connection);
    }
    let view = clients(&registry, 1, Sort::Bytes);
    assert!(view.truncated);
    assert_eq!(view.clients[0].ip.to_string(), "198.51.100.3");
    assert_eq!(view.other.clients, 2);
    assert_eq!(view.other.bytes_out, 55);
    assert_eq!(view.totals.clients, 3);
    assert_eq!(view.totals.bytes_out, 555);
}

#[test]
fn the_ip_prefix_filters_rows_but_not_the_totals() {
    let registry = registry(100, 128);
    let _kept = open(&registry, "203.0.113.7:1", "edge", Protocol::Http1);
    let _dropped = open(&registry, "198.51.100.1:1", "edge", Protocol::Http1);
    let view = registry.clients(
        &ClientQuery {
            limit: 10,
            sort: Sort::Recent,
            ip_prefix: Some("203.0.113.".into()),
        },
        Instant::now(),
    );
    assert_eq!(view.clients.len(), 1);
    assert_eq!(view.totals.clients, 2);
    assert_eq!(view.tracked_clients, 2);
}

#[test]
fn the_least_recently_active_clients_are_evicted_beyond_the_cap_but_stay_in_the_totals() {
    let registry = registry(4, 128);
    for index in 1..=40_u8 {
        let connection = open(
            &registry,
            &format!("192.0.2.{index}:1"),
            "edge",
            Protocol::Http1,
        );
        connection.stats().sent::<Exclusive>(10, Instant::now());
        drop(connection);
    }
    let view = clients(&registry, 100, Sort::Recent);
    assert!(view.tracked_clients <= 4, "{}", view.tracked_clients);
    assert_eq!(view.totals.bytes_out, 400);
    let newest = view.clients.first().expect("a client");
    assert_eq!(newest.ip.to_string(), "192.0.2.40");
    assert_eq!(view.other.bytes_out, 400 - 10 * view.clients.len() as u64);
}

#[test]
fn live_connections_survive_the_cap_while_idle_history_is_shed() {
    let registry = registry(2, 128);
    let live = open(&registry, "192.0.2.1:1", "edge", Protocol::Http1);
    for index in 2..=10_u8 {
        drop(open(
            &registry,
            &format!("192.0.2.{index}:1"),
            "edge",
            Protocol::Http1,
        ));
    }
    let view = clients(&registry, 100, Sort::Connections);
    assert!(
        view.clients
            .iter()
            .any(|row| row.ip.to_string() == "192.0.2.1")
    );
    assert_eq!(view.clients[0].connections, 1);
    drop(live);
}

#[test]
fn connections_list_the_peer_the_protocol_and_the_state_inputs() {
    let registry = registry(100, 64);
    let tls = open(&registry, "[2001:db8::9]:5000", "secure", Protocol::Http1);
    tls.stats().set_handshaking(true);
    tls.stats().set_tls(TlsInfo {
        version: "TLS1.3",
        sni: Some("example.com".into()),
        alpn: Some("h2".into()),
    });
    tls.stats().set_protocol(Protocol::H2);
    tls.stats().set_upstream(Arc::from("192.0.2.80:8080"));
    let plain = open(&registry, "203.0.113.7:4000", "edge", Protocol::Tcp);
    let view = connections(&registry, 10);
    assert_eq!((view.total, view.truncated), (2, false));
    let row = view
        .connections
        .iter()
        .find(|row| row.id == tls.stats().id())
        .expect("the connection is listed");
    assert_eq!(row.peer.to_string(), "[2001:db8::9]:5000");
    assert_eq!(row.protocol, Protocol::H2);
    assert!(row.handshaking);
    assert_eq!(
        row.tls.as_ref().and_then(|tls| tls.sni.as_deref()),
        Some("example.com")
    );
    assert_eq!(row.upstream.as_deref(), Some("192.0.2.80:8080"));
    assert_ne!(plain.stats().id(), tls.stats().id());
}

#[test]
fn connection_listing_filters_and_truncates_oldest_first() {
    let registry = registry(100, 128);
    let first = open(&registry, "203.0.113.7:1", "edge", Protocol::Http1);
    let _second = open(&registry, "203.0.113.8:1", "edge", Protocol::Http1);
    let _third = open(&registry, "198.51.100.1:1", "other", Protocol::Http1);
    let listed = connections(&registry, 1);
    assert_eq!((listed.total, listed.truncated), (3, true));
    assert_eq!(listed.connections[0].id, first.stats().id());
    let filtered = registry.connections(
        &ConnectionQuery {
            limit: 10,
            ip_prefix: Some("203.0.113.".into()),
            listener: Some("edge".into()),
        },
        Instant::now(),
    );
    assert_eq!(filtered.total, 2);
}

#[test]
fn a_disabled_registry_tracks_nothing_and_a_reload_applies_new_settings() {
    let registry = registry(100, 128);
    registry.activate(Settings {
        enabled: false,
        ..registry.settings()
    });
    assert!(
        registry
            .open(Arrival {
                peer: "203.0.113.7:1".parse().expect("a socket address"),
                listener: Arc::from("edge"),
                protocol: Protocol::Http1,
                now: Instant::now(),
            })
            .is_none()
    );
    registry.activate(Settings::default());
    let _connection = open(&registry, "203.0.113.7:1", "edge", Protocol::Http1);
    assert_eq!(connections(&registry, 10).total, 1);
}

#[test]
fn connection_ids_are_unique_and_slots_are_reused() {
    let registry = registry(100, 128);
    let first = open(&registry, "203.0.113.7:1", "edge", Protocol::Http1);
    let first_id = first.stats().id();
    drop(first);
    let second = open(&registry, "203.0.113.7:2", "edge", Protocol::Http1);
    assert_ne!(second.stats().id(), first_id);
    assert_eq!(connections(&registry, 10).total, 1);
}
