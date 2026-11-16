use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::atomic::Ordering,
    time::Duration,
};

use crate::support::{ClientSettings, Rig, Settings, eventually};
use oxiroute_epoll_h1::Timeouts;
use rustls::{HandshakeKind, version};

const TLS12: &[&rustls::SupportedProtocolVersion] = &[&version::TLS12];
const TLS13: &[&rustls::SupportedProtocolVersion] = &[&version::TLS13];

fn default_rig() -> Rig {
    Rig::start(&Settings::default(), ClientSettings::default())
}

fn at_end(wire: &mut crate::support::Wire<crate::support::Tls>) -> bool {
    let mut byte = [0_u8; 1];
    matches!(wire.stream.read(&mut byte), Ok(0))
}

#[test]
fn a_connection_the_server_closes_ends_with_close_notify() {
    let rig = default_rig();
    let mut wire = rig.wire();
    wire.send(b"GET /size/10 HTTP/1.1\r\nHost: test.example\r\nConnection: close\r\n\r\n");
    assert_eq!(wire.reply().body.len(), 10);
    assert!(at_end(&mut wire));
}

#[test]
fn a_close_notify_from_the_client_is_answered_with_one() {
    let rig = default_rig();
    let mut wire = rig.wire();
    wire.get("/size/10");
    assert_eq!(wire.reply().body.len(), 10);
    wire.stream.conn.send_close_notify();
    wire.stream.flush().expect("flush");
    assert!(at_end(&mut wire));
    eventually("the connection to be released", || {
        rig.fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst)
            == 0
    });
}

#[test]
fn a_connection_dropped_without_close_notify_releases_its_admission() {
    let rig = default_rig();
    let mut wire = rig.wire();
    wire.get("/size/10");
    assert_eq!(wire.reply().body.len(), 10);
    drop(wire);
    eventually("the connection to be released", || {
        rig.fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst)
            == 0
    });
}

#[test]
fn a_draining_engine_closes_idle_tls_connections_with_close_notify() {
    let rig = default_rig();
    let mut wire = rig.wire();
    wire.get("/size/10");
    assert_eq!(wire.reply().body.len(), 10);
    rig.fixture.engine().drain();
    assert!(at_end(&mut wire));
}

#[test]
fn sessions_resume_with_tls_12_and_tls_13_with_cache_and_with_tickets() {
    for versions in [TLS12, TLS13] {
        for tickets in [false, true] {
            let rig = Rig::start(
                &Settings {
                    versions,
                    tickets,
                    ..Settings::default()
                },
                ClientSettings {
                    versions,
                    ..ClientSettings::default()
                },
            );
            let mut first = rig.wire();
            assert_eq!(
                first.stream.conn.handshake_kind(),
                Some(HandshakeKind::Full)
            );
            first.get("/size/10");
            assert_eq!(first.reply().body.len(), 10);
            let mut second = rig.wire();
            assert_eq!(
                second.stream.conn.handshake_kind(),
                Some(HandshakeKind::Resumed),
                "{versions:?} tickets={tickets}"
            );
            second.get("/size/10");
            assert_eq!(second.reply().body.len(), 10);
        }
    }
}

#[test]
fn plain_http_on_a_tls_listener_is_closed_and_never_handed_off() {
    let rig = default_rig();
    let mut socket = TcpStream::connect(rig.fixture.addr).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    socket
        .write_all(b"GET /size/1 HTTP/1.1\r\nHost: test.example\r\n\r\n")
        .expect("send");
    let mut rest = Vec::new();
    let _ = socket.read_to_end(&mut rest);
    assert!(!rest.starts_with(b"HTTP/"));
    assert!(rig.fixture.handoffs.try_recv().is_err());
    eventually("the connection to be released", || {
        rig.fixture
            .service
            .counters
            .live_admissions
            .load(Ordering::SeqCst)
            == 0
    });
}

#[test]
fn a_connection_that_never_finishes_its_handshake_is_closed_at_the_first_request_deadline() {
    let timeouts = Timeouts {
        first_request: Some(Duration::from_millis(300)),
        ..Timeouts::default()
    };
    let rig = Rig::with_timeouts(&Settings::default(), ClientSettings::default(), timeouts);
    let mut socket = TcpStream::connect(rig.fixture.addr).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    socket.write_all(&[0x16, 0x03, 0x01]).expect("send");
    let mut rest = Vec::new();
    assert!(socket.read_to_end(&mut rest).is_ok());
}
