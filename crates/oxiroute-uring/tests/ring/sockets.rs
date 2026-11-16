use std::{net::TcpListener, os::fd::OwnedFd, time::Duration};

use crate::support::{Harness, client, read_all, small, write_all};
use oxiroute_uring::{Config, Event, Read, Sent, Taskrun};
use rustix::{
    event::{EventfdFlags, eventfd},
    net::{AddressFamily, SocketFlags, SocketType, socket_with},
};

#[test]
fn a_ring_configured_like_the_plane_passes_its_own_probe() {
    oxiroute_uring::probe(&Config::default()).expect("this kernel runs the data plane");
    oxiroute_uring::probe(&Config {
        taskrun: Taskrun::Cooperative,
        ..Config::default()
    })
    .expect("cooperative task running works too");
}

#[test]
fn an_accepted_connection_echoes_what_it_receives() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, from) = harness.accept(7);
    assert_eq!(from, peer.local_addr().expect("local address"));
    write_all(&peer, b"hello ring");
    harness.readable(7);
    let mut bytes = [0_u8; 64];
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Data(10));
    assert_eq!(&bytes[..10], b"hello ring");
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Block);
    assert_eq!(
        harness.ring.send(handle, b"echo: ", b"hello ring"),
        Sent::Queued(16)
    );
    harness.pump(10);
    let mut reply = [0_u8; 16];
    std::io::Read::read_exact(&mut &peer, &mut reply).expect("reply");
    assert_eq!(&reply, b"echo: hello ring");
}

#[test]
fn a_chunk_larger_than_the_reader_is_taken_in_parts_in_order() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(1);
    let payload: Vec<u8> = (0..9000_u32).map(|n| (n % 251) as u8).collect();
    write_all(&peer, &payload);
    harness.readable(1);
    assert_eq!(harness.read_exact(handle, 1, payload.len()), payload);
}

#[test]
fn the_peer_closing_is_reported_after_the_bytes_it_sent() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(3);
    write_all(&peer, b"last words");
    harness.readable(3);
    drop(peer);
    let mut bytes = [0_u8; 32];
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Data(10));
    harness.until("end of file", |event| {
        matches!(event, Event::Eof { tag: 3 })
    });
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Eof);
}

#[test]
fn a_second_send_waits_for_the_first_and_is_announced_when_it_may_go() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(5);
    assert_eq!(harness.ring.send(handle, b"one", b""), Sent::Queued(3));
    assert_eq!(harness.ring.send(handle, b"two", b""), Sent::Busy);
    harness.until("writable", |event| {
        matches!(event, Event::Writable { tag: 5 })
    });
    assert_eq!(harness.ring.send(handle, b"two", b""), Sent::Queued(3));
    harness.ring.close(handle);
    harness.settle();
    assert_eq!(read_all(&peer), b"onetwo");
}

#[test]
fn a_write_over_the_staging_limit_is_accepted_in_parts_head_first() {
    let config = Config {
        send_limit: 1000,
        ..small()
    };
    let mut harness = Harness::new(config);
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(9);
    let (head, body) = (vec![1_u8; 1500], vec![2_u8; 2000]);
    let mut sent = 0;
    while sent < head.len() + body.len() {
        let from_head = sent.min(head.len());
        match harness
            .ring
            .send(handle, &head[from_head..], &body[sent - from_head..])
        {
            Sent::Queued(count) => sent += count,
            Sent::Busy => {
                harness.until("writable", |event| {
                    matches!(event, Event::Writable { tag: 9 })
                });
            }
            Sent::Failed => panic!("send failed"),
        }
    }
    harness.ring.close(handle);
    harness.settle();
    assert_eq!(read_all(&peer), [head, body].concat());
}

#[test]
fn accepting_can_be_paused_and_the_connections_made_meanwhile_arrive_on_resume() {
    let mut harness = Harness::new(small());
    let (listener, address) = harness.listener();
    harness.pump(50);
    harness.ring.set_accepting(listener, false);
    harness.pump(50);
    let _waiting = client(address);
    for _ in 0..4 {
        harness.pump(25);
    }
    assert!(
        !harness
            .events
            .iter()
            .any(|event| matches!(event, Event::Accepted { .. }))
    );
    harness.ring.set_accepting(listener, true);
    let (_, peer) = harness.accept(2);
    assert!(peer.ip().is_loopback());
}

#[test]
fn a_connect_reports_success_and_failure() {
    let mut harness = Harness::new(small());
    let origin = TcpListener::bind("127.0.0.1:0").expect("bind");
    let target = origin.local_addr().expect("address");
    let open = socket_with(
        AddressFamily::INET,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("socket");
    let handle = harness.ring.adopt(open, 4);
    assert_eq!(harness.ring.connected(handle), None);
    harness.ring.connect(handle, target);
    harness.until("connected", |event| {
        matches!(event, Event::Connected { tag: 4 })
    });
    assert_eq!(harness.ring.connected(handle), Some(true));
    let (mut accepted, _) = origin.accept().expect("accept");
    assert_eq!(harness.ring.send(handle, b"ping", b""), Sent::Queued(4));
    harness.pump(10);
    let mut bytes = [0_u8; 4];
    std::io::Read::read_exact(&mut accepted, &mut bytes).expect("ping");
    assert_eq!(&bytes, b"ping");

    let closed = {
        let socket = TcpListener::bind("127.0.0.1:0").expect("bind");
        socket.local_addr().expect("address")
    };
    let refused = socket_with(
        AddressFamily::INET,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("socket");
    let failing = harness.ring.adopt(refused, 6);
    harness.ring.connect(failing, closed);
    harness.until("failed", |event| matches!(event, Event::Failed { tag: 6 }));
    assert_eq!(harness.ring.connected(failing), Some(false));
}

#[test]
fn a_descriptor_that_is_not_a_socket_can_be_waited_on() {
    let mut harness = Harness::new(small());
    let wake: OwnedFd =
        eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).expect("eventfd");
    let writer = wake.try_clone().expect("clone");
    let handle = harness.ring.adopt(wake, 8);
    harness.ring.notify(handle);
    harness.pump(30);
    assert!(harness.events.is_empty());
    rustix::io::write(&writer, &1_u64.to_ne_bytes()).expect("write");
    harness.until("notification", |event| {
        matches!(event, Event::Notified { tag: 8 })
    });
}

#[test]
fn shutting_down_the_write_side_follows_the_send_in_flight() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(2);
    assert_eq!(harness.ring.send(handle, b"bye", b""), Sent::Queued(3));
    harness.ring.shutdown_write(handle);
    harness.pump(10);
    assert_eq!(read_all(&peer), b"bye");
    harness.ring.close(handle);
}

#[test]
fn a_stale_handle_is_ignored() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let _peer = client(address);
    let (handle, _) = harness.accept(1);
    harness.ring.close(handle);
    harness.settle();
    let mut bytes = [0_u8; 8];
    assert_eq!(harness.ring.read(handle, &mut bytes), Read::Failed);
    assert_eq!(harness.ring.send(handle, b"x", b""), Sent::Failed);
    harness.ring.close(handle);
    harness.pump(10);
    harness.settle();
}

#[test]
fn a_slow_reader_gets_everything_without_the_ring_blocking() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(1);
    let total = 6 * 1024 * 1024;
    let payload = vec![0x5A_u8; 64 * 1024];
    let reader = std::thread::spawn(move || {
        let mut got = 0;
        let mut buffer = vec![0_u8; 8192];
        while got < total {
            std::thread::sleep(Duration::from_micros(200));
            got += std::io::Read::read(&mut &peer, &mut buffer).expect("read");
        }
        got
    });
    let mut sent = 0;
    while sent < total {
        match harness
            .ring
            .send(handle, &[], &payload[..payload.len().min(total - sent)])
        {
            Sent::Queued(count) => sent += count,
            Sent::Busy => {
                harness.until("writable", |event| {
                    matches!(event, Event::Writable { tag: 1 })
                });
            }
            Sent::Failed => panic!("send failed"),
        }
    }
    harness.ring.close(handle);
    harness.settle();
    assert_eq!(reader.join().expect("reader"), total);
}
