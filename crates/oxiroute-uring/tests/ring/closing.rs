use std::{
    io::{ErrorKind, Read as _, Write as _},
    net::TcpStream,
    time::{Duration, Instant},
};

use crate::support::{Harness, client, read_all, small, write_all};
use oxiroute_uring::{Config, Event, Read, Sent};
use rustix::net::sockopt;

fn shrink_send_buffer(stream: &TcpStream) {
    sockopt::set_socket_recv_buffer_size(stream, 65536).expect("receive buffer");
}

#[test]
fn closing_a_socket_with_a_receive_in_flight_cancels_it_and_the_peer_sees_the_end() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(1);
    harness.pump(20);
    harness.ring.close(handle);
    harness.settle();
    assert_eq!(read_all(&peer), b"");
    assert!(
        harness
            .events
            .iter()
            .all(|event| !matches!(event, Event::Eof { tag: 1 } | Event::Failed { tag: 1 }))
    );
}

#[test]
fn a_completion_that_lands_after_the_close_gives_its_buffer_back() {
    let config = Config {
        buffers: 2,
        ..small()
    };
    let mut harness = Harness::new(config);
    let (_listener, address) = harness.listener();
    for round in 0..200_u64 {
        let peer = client(address);
        let (handle, _) = harness.accept(round + 10);
        write_all(&peer, b"data that arrives while the socket is being closed");
        harness.pump(2);
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
fn bytes_the_ring_accepted_are_flushed_before_the_close_takes_the_connection() {
    let mut harness = Harness::new(Config {
        send_limit: 4 * 1024 * 1024,
        ..small()
    });
    let (_listener, address) = harness.listener();
    let peer = client(address);
    shrink_send_buffer(&peer);
    let (handle, _) = harness.accept_with(1, Some(65536));
    let payload: Vec<u8> = (0..3_000_000_u32).map(|n| (n % 253) as u8).collect();
    assert_eq!(
        harness.ring.send(handle, &[], &payload),
        Sent::Queued(payload.len())
    );
    harness.pump(10);
    harness.ring.close(handle);
    harness.pump(10);
    assert!(
        harness.ring.in_flight() > harness.baseline,
        "the flush is still running"
    );
    let reader = std::thread::spawn(move || read_all(&peer));
    harness.settle();
    let received = reader.join().expect("reader");
    assert_eq!(received.len(), payload.len());
    assert!(received == payload, "the flushed bytes differ");
}

#[test]
fn a_flush_that_the_peer_never_drains_is_cancelled_when_its_time_is_up() {
    let mut harness = Harness::new(Config {
        send_limit: 8 * 1024 * 1024,
        flush: Duration::from_millis(300),
        ..small()
    });
    let (_listener, address) = harness.listener();
    let peer = client(address);
    shrink_send_buffer(&peer);
    let (handle, _) = harness.accept_with(1, Some(65536));
    let payload = vec![7_u8; 8 * 1024 * 1024];
    assert_eq!(
        harness.ring.send(handle, &[], &payload),
        Sent::Queued(payload.len())
    );
    harness.pump(10);
    harness.ring.close(handle);
    let started = Instant::now();
    harness.settle();
    assert!(started.elapsed() >= Duration::from_millis(250));
    peer.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut received = 0;
    let mut buffer = vec![0_u8; 65536];
    loop {
        match (&peer).read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => received += count,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => break,
            Err(error) => panic!("unexpected read error: {error}"),
        }
    }
    assert!(
        received < payload.len(),
        "the cancelled flush stopped short"
    );
}

#[test]
fn a_released_socket_goes_back_to_its_owner_with_no_operation_left_on_it() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let mut peer = client(address);
    let (handle, _) = harness.accept(1);
    harness.pump(20);
    let fd = harness.ring.release(handle).expect("released");
    assert_eq!(harness.ring.in_flight(), harness.baseline);
    let mut stream = TcpStream::from(fd);
    peer.write_all(b"after the release").expect("write");
    let mut bytes = [0_u8; 17];
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    stream
        .read_exact(&mut bytes)
        .expect("the owner receives what the ring never took");
    assert_eq!(&bytes, b"after the release");
}

#[test]
fn a_release_waits_for_the_send_in_flight_and_loses_nothing() {
    let mut harness = Harness::new(small());
    let (_listener, address) = harness.listener();
    let mut peer = client(address);
    let (handle, _) = harness.accept(1);
    assert_eq!(
        harness.ring.send(handle, b"previous response", b""),
        Sent::Queued(17)
    );
    let fd = harness.ring.release(handle).expect("released");
    let mut bytes = [0_u8; 17];
    peer.read_exact(&mut bytes).expect("response");
    assert_eq!(&bytes, b"previous response");
    drop(fd);
}

#[test]
fn a_release_that_cannot_finish_its_send_fails_and_closes_the_connection() {
    let mut harness = Harness::new(Config {
        send_limit: 8 * 1024 * 1024,
        flush: Duration::from_millis(200),
        ..small()
    });
    let (_listener, address) = harness.listener();
    let peer = client(address);
    shrink_send_buffer(&peer);
    let (handle, _) = harness.accept_with(1, Some(65536));
    let payload = vec![1_u8; 8 * 1024 * 1024];
    assert_eq!(
        harness.ring.send(handle, &[], &payload),
        Sent::Queued(payload.len())
    );
    harness.pump(10);
    let error = harness
        .ring
        .release(handle)
        .expect_err("the send cannot finish");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    harness.settle();
}

#[test]
fn closing_a_listener_with_accepts_in_flight_cancels_them_and_frees_the_port() {
    let mut harness = Harness::new(small());
    let (listener, address) = harness.listener();
    harness.pump(20);
    harness.ring.close(listener);
    harness.baseline = 0;
    harness.settle();
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_err());
}
