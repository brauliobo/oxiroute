use std::{collections::HashMap, net::TcpStream};

use crate::support::{Harness, client, small, write_all};
use oxiroute_uring::{Config, Event, Read, Sent};

fn echo_round(
    harness: &mut Harness,
    handles: &[(u64, oxiroute_uring::Handle, TcpStream)],
    rounds: usize,
) {
    let mut seen: HashMap<u64, Vec<u8>> = HashMap::new();
    for round in 0..rounds {
        for (tag, _, peer) in handles {
            write_all(peer, format!("request {round} of {tag}").as_bytes());
        }
        let wanted: usize = handles
            .iter()
            .map(|(tag, _, _)| format!("request {round} of {tag}").len())
            .sum();
        let mut got = 0;
        let mut buffer = [0_u8; 128];
        while got < wanted {
            harness.pump(50);
            let events = std::mem::take(&mut harness.events);
            for event in events {
                let Event::Readable { tag } = event else {
                    continue;
                };
                let Some((_, handle, _)) =
                    handles.iter().find(|(candidate, _, _)| *candidate == tag)
                else {
                    continue;
                };
                while let Read::Data(count) = harness.ring.read(*handle, &mut buffer) {
                    seen.entry(tag)
                        .or_default()
                        .extend_from_slice(&buffer[..count]);
                    got += count;
                }
            }
        }
    }
    for (tag, _, _) in handles {
        let expected: String = (0..rounds)
            .map(|round| format!("request {round} of {tag}"))
            .collect();
        assert_eq!(seen[tag], expected.as_bytes(), "connection {tag}");
    }
}

fn connect_all(harness: &mut Harness, count: u64) -> Vec<(u64, oxiroute_uring::Handle, TcpStream)> {
    let (_listener, address) = harness.listener();
    (100..100 + count)
        .map(|tag| {
            let peer = client(address);
            let (handle, from) = harness.accept(tag);
            assert_eq!(from, peer.local_addr().expect("local address"));
            (tag, handle, peer)
        })
        .collect()
}

#[test]
fn a_ring_with_two_buffers_serves_sixty_four_connections_through_buffer_exhaustion() {
    let mut harness = Harness::new(Config {
        buffers: 2,
        ..small()
    });
    let connections = connect_all(&mut harness, 64);
    echo_round(&mut harness, &connections, 5);
}

#[test]
fn a_thousand_connections_are_accepted_read_and_closed_without_leaking_operations() {
    let mut harness = Harness::new(Config {
        entries: 4096,
        buffers: 1024,
        ..small()
    });
    let connections = connect_all(&mut harness, 1000);
    echo_round(&mut harness, &connections, 2);
    for (_, handle, _) in &connections {
        harness.ring.close(*handle);
    }
    harness.settle();
    drop(connections);
    assert_eq!(harness.ring.in_flight(), harness.baseline);
}

#[test]
fn many_slow_peers_hold_no_more_than_one_buffer_each_and_the_rest_still_flow() {
    let mut harness = Harness::new(Config {
        buffers: 8,
        ..small()
    });
    let slow = connect_all(&mut harness, 8);
    for (_, _, peer) in &slow {
        write_all(peer, b"parked");
    }
    for _ in 0..4 {
        harness.pump(25);
    }
    let (fast_listener, address) = harness.listener();
    let _ = fast_listener;
    let fast = client(address);
    let (handle, _) = harness.accept(5000);
    write_all(&fast, b"overtakes");
    for _ in 0..8 {
        harness.pump(25);
    }
    let mut bytes = [0_u8; 16];
    let mut taken = 0;
    for (_, slow_handle, _) in &slow {
        if let Read::Data(count) = harness.ring.read(*slow_handle, &mut bytes) {
            taken += count;
        }
    }
    assert_eq!(taken, 6 * 8);
    harness.until("the fast peer", |event| {
        matches!(event, Event::Readable { tag: 5000 })
    });
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Data(9));
    assert_eq!(harness.ring.send(handle, b"ok", b""), Sent::Queued(2));
}

fn reset(peer: TcpStream) {
    rustix::net::sockopt::set_socket_linger(&peer, Some(std::time::Duration::ZERO))
        .expect("linger");
    drop(peer);
}

#[test]
fn a_starved_socket_that_fails_does_not_strand_the_starved_sockets_behind_it() {
    let mut harness = Harness::new(Config {
        buffers: 2,
        ..small()
    });
    let mut connections = connect_all(&mut harness, 12);
    for (_, _, peer) in &connections[..2] {
        write_all(peer, b"hold");
    }
    for _ in 0..4 {
        harness.pump(25);
    }
    let (_, _, broken) = connections.remove(2);
    reset(broken);
    for (_, _, peer) in &connections[2..] {
        write_all(peer, b"data");
    }
    for _ in 0..4 {
        harness.pump(25);
    }
    let mut bytes = [0_u8; 16];
    for (_, handle, _) in &connections[..2] {
        assert_eq!(harness.ring.read(*handle, &mut bytes), Read::Data(4));
    }
    let mut waiting: Vec<_> = connections[2..]
        .iter()
        .map(|(tag, handle, _)| (*tag, *handle))
        .collect();
    while !waiting.is_empty() {
        let Event::Readable { tag } = harness.until("a readable socket", |event| {
            matches!(event, Event::Readable { tag } if waiting.iter().any(|(candidate, _)| candidate == tag))
        }) else {
            unreachable!()
        };
        let at = waiting
            .iter()
            .position(|(candidate, _)| *candidate == tag)
            .expect("waiting");
        let (_, handle) = waiting.remove(at);
        assert_eq!(harness.ring.read(handle, &mut bytes), Read::Data(4));
    }
}

#[test]
fn connections_reset_by_the_peer_do_not_leak_receive_buffers() {
    let mut harness = Harness::new(Config {
        buffers: 2,
        ..small()
    });
    let (_listener, address) = harness.listener();
    for round in 0..300_u64 {
        let peer = client(address);
        let (handle, _) = harness.accept(10 + round);
        write_all(&peer, b"about to be reset");
        harness.pump(1);
        reset(peer);
        harness.pump(1);
        if round % 2 == 1 {
            let mut bytes = [0_u8; 32];
            let _ = harness.ring.read(handle, &mut bytes);
        }
        harness.ring.close(handle);
        harness.settle();
    }
    let peer = client(address);
    let (handle, _) = harness.accept(1);
    write_all(&peer, b"still served");
    harness.readable(1);
    let mut bytes = [0_u8; 32];
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Data(12));
}

#[test]
fn bytes_nobody_reads_never_pin_the_whole_buffer_pool() {
    let mut harness = Harness::new(Config {
        buffers: 8,
        ..small()
    });
    let unread = connect_all(&mut harness, 32);
    for (_, _, peer) in &unread {
        write_all(peer, b"unread");
    }
    for _ in 0..6 {
        harness.pump(25);
    }
    let (_listener, address) = harness.listener();
    let fresh = client(address);
    let (handle, _) = harness.accept(9000);
    write_all(&fresh, b"gets through");
    harness.readable(9000);
    let mut bytes = [0_u8; 32];
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Data(12));
    for (_, handle, _) in &unread {
        assert_eq!(harness.ring.read(*handle, &mut bytes), Read::Data(6));
        assert_eq!(&bytes[..6], b"unread");
    }
}
