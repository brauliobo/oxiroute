#![allow(clippy::pedantic)]

mod support;

use std::{collections::HashMap, net::TcpStream};

use oxiroute_uring::{Config, Event, Read, Sent};
use support::{Harness, client, small, write_all};

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
