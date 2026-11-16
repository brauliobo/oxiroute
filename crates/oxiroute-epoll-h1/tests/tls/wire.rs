use std::{io::Write, sync::atomic::Ordering, thread, time::Duration};

use crate::support::{ClientSettings, Rig, Settings, eventually};
use rustls::{ProtocolVersion, version};

const VERSIONS: [(&[&rustls::SupportedProtocolVersion], ProtocolVersion); 2] = [
    (&[&version::TLS12], ProtocolVersion::TLSv1_2),
    (&[&version::TLS13], ProtocolVersion::TLSv1_3),
];

fn rig(versions: &'static [&'static rustls::SupportedProtocolVersion]) -> Rig {
    Rig::start(
        &Settings {
            versions,
            ..Settings::default()
        },
        ClientSettings {
            versions,
            ..ClientSettings::default()
        },
    )
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| b'a' + (index % 23) as u8).collect()
}

#[test]
fn a_request_is_proxied_over_tls_12_and_tls_13() {
    for (versions, expected) in VERSIONS {
        let rig = rig(versions);
        let mut wire = rig.wire();
        assert_eq!(wire.stream.conn.protocol_version(), Some(expected));
        wire.get("/size/5000");
        let reply = wire.reply();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, pattern(5000));
        assert!(reply.header("x-request-id").is_some());
    }
}

#[test]
fn bodies_larger_than_the_buffers_cross_in_both_directions() {
    for (versions, _) in VERSIONS {
        let rig = rig(versions);
        let mut wire = rig.wire();
        let body = pattern(3 * 1024 * 1024);
        let head = format!(
            "POST /echo HTTP/1.1\r\nHost: test.example\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        wire.send(head.as_bytes());
        wire.send(&body);
        assert_eq!(wire.reply().body, body);
        wire.get("/size/9000000");
        assert_eq!(wire.reply().body, pattern(9_000_000));
    }
}

#[test]
fn a_reader_slower_than_the_origin_receives_every_byte() {
    let rig = rig(rustls::ALL_VERSIONS);
    let mut wire = rig.wire();
    wire.get("/size/6000000");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(wire.reply().body, pattern(6_000_000));
    wire.get("/size/10");
    assert_eq!(wire.reply().body, pattern(10));
}

#[test]
fn requests_share_one_client_connection_and_one_origin_connection() {
    let rig = rig(rustls::ALL_VERSIONS);
    let mut wire = rig.wire();
    for index in 0..50 {
        wire.get(&format!("/size/{index}"));
        assert_eq!(wire.reply().body.len(), index);
    }
    eventually("the exchanges", || {
        rig.fixture
            .service
            .counters
            .completed
            .load(Ordering::SeqCst)
            == 50
    });
    assert_eq!(
        rig.fixture.service.counters.admitted.load(Ordering::SeqCst),
        1
    );
    assert_eq!(rig.fixture.origin.accepted(), 1);
}

#[test]
fn pipelined_requests_in_one_record_are_answered_in_order() {
    let rig = rig(rustls::ALL_VERSIONS);
    let mut wire = rig.wire();
    let requests = (1..=3)
        .map(|size| format!("GET /size/{size}00 HTTP/1.1\r\nHost: test.example\r\n\r\n"))
        .collect::<String>();
    wire.send(requests.as_bytes());
    for size in 1..=3 {
        assert_eq!(wire.reply().body.len(), size * 100);
    }
}

#[test]
fn a_handshake_and_a_request_written_one_byte_at_a_time_are_served() {
    let rig = rig(rustls::ALL_VERSIONS);
    let mut wire = rig.client.trickle("a.example").expect("a TLS connection");
    wire.send(b"GET /size/300 HTTP/1.1\r\nHost: test.example\r\n\r\n");
    wire.stream.flush().expect("flush");
    assert_eq!(wire.reply().body, pattern(300));
}
