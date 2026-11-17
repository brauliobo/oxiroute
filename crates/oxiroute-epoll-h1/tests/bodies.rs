#![allow(clippy::pedantic)]

mod support;

use std::{thread, time::Duration};

use oxiroute_epoll_h1::{Failure, Timeouts};
use support::{Fixture, Knobs, eventually, sized, test_config};

fn recorded() -> Fixture {
    let knobs = Knobs {
        record: true,
        ..Knobs::default()
    };
    Fixture::with(test_config(), Timeouts::default(), knobs)
}

fn chunked_text(chunks: usize) -> Vec<u8> {
    (0..chunks)
        .flat_map(|index| format!("chunk-{index};").into_bytes())
        .collect()
}

#[test]
fn a_body_far_larger_than_the_buffers_reaches_a_client_that_reads_slowly_intact() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    rustix::net::sockopt::set_socket_recv_buffer_size(&wire.stream, 16384).expect("small buffer");
    wire.get("/size/2000000");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(wire.reply().body, sized(2_000_000));
    wire.get("/size/10");
    assert_eq!(wire.reply().body, sized(10));
}

#[test]
fn a_client_that_pauses_between_reads_gets_every_byte_in_order() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    rustix::net::sockopt::set_socket_recv_buffer_size(&wire.stream, 8192).expect("small buffer");
    wire.get("/size/400000");
    let head = wire.head();
    assert_eq!(head.status, 200);
    thread::sleep(Duration::from_millis(200));
    let body = wire.rest();
    assert_eq!(body, sized(400_000));
}

#[test]
fn an_origin_that_closes_inside_a_body_ends_the_client_connection_and_the_engine_goes_on() {
    let fixture = recorded();
    let mut wire = fixture.wire();
    wire.get("/truncated/200000");
    assert_eq!(wire.head().status, 200);
    let body = wire.rest();
    assert!(body.len() <= 200_000, "{}", body.len());
    assert!(
        body.iter()
            .zip(sized(body.len()))
            .all(|(got, want)| *got == want)
    );
    eventually("the upstream failure", || {
        fixture
            .outcomes()
            .last()
            .is_some_and(|outcome| outcome.failure == Some(Failure::UpstreamClosed))
    });
    let mut next = fixture.wire();
    next.get("/size/100000");
    assert_eq!(next.reply().body, sized(100_000));
}

#[test]
fn a_chunked_body_of_many_chunks_larger_than_the_buffers_arrives_whole() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/chunked/20000");
    assert_eq!(wire.reply().body, chunked_text(20_000));
    wire.get("/chunked/3");
    assert_eq!(wire.reply().body, chunked_text(3));
    assert_eq!(fixture.origin.accepted(), 1);
}

#[test]
fn a_body_that_dribbles_in_small_pieces_arrives_whole() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    wire.get("/dribble/300000");
    assert_eq!(wire.reply().body, sized(300_000));
    wire.get("/size/10");
    assert_eq!(wire.reply().body, sized(10));
}

#[test]
fn bytes_after_a_length_delimited_body_keep_the_origin_connection_from_being_reused() {
    let fixture = Fixture::start();
    let mut wire = fixture.wire();
    for _ in 0..3 {
        wire.get("/trailing/70000");
        assert_eq!(wire.reply().body, sized(70_000));
    }
    assert_eq!(fixture.origin.accepted(), 3);
}

#[test]
fn many_clients_pulling_large_bodies_at_once_each_get_their_own_bytes() {
    let fixture = Fixture::start();
    let clients: Vec<_> = (0..16_usize)
        .map(|index| {
            let mut wire = fixture.wire();
            thread::spawn(move || {
                let size = 200_000 + index * 7_000;
                wire.get(&format!("/size/{size}"));
                assert_eq!(wire.reply().body, sized(size));
            })
        })
        .collect();
    clients
        .into_iter()
        .for_each(|client| client.join().expect("client"));
}
