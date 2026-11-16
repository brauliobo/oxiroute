use std::{
    io::{Read, Write},
    time::Duration,
};

use crate::support::{ClientSettings, Guard, Identity, Rig, Settings, Wire};
use oxiroute_epoll_h1::Handoff;
use rustls::{ServerConnection, StreamOwned};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

fn handed_off(rig: &Rig) -> Handoff<Guard> {
    rig.fixture
        .handoffs
        .recv_timeout(Duration::from_secs(5))
        .expect("a handoff")
}

fn session_of(
    handoff: Handoff<Guard>,
) -> (StreamOwned<ServerConnection, std::net::TcpStream>, Vec<u8>) {
    handoff.stream.set_nonblocking(false).expect("blocking");
    handoff
        .stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let conn = handoff.tls.expect("the established TLS session travels");
    (StreamOwned::new(*conn, handoff.stream), handoff.prefix)
}

fn read_exactly(stream: &mut impl Read, len: usize) -> Vec<u8> {
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).expect("bytes");
    bytes
}

fn serving(alpn: Vec<&'static [u8]>, client: Vec<&'static [u8]>) -> Rig {
    Rig::start(
        &Settings {
            alpn,
            ..Settings::default()
        },
        ClientSettings {
            alpn: client,
            ..ClientSettings::default()
        },
    )
}

#[test]
fn sni_selects_the_certificate_of_the_requested_name() {
    let rig = Rig::start(
        &Settings {
            names: vec!["a.example", "b.example"],
            ..Settings::default()
        },
        ClientSettings::default(),
    );
    for identity in &rig.server.identities {
        let mut wire = rig.client.connect(&identity.name).expect("connect");
        let presented = wire.stream.conn.peer_certificates().expect("certificates");
        assert_eq!(presented[0], identity.cert);
        wire.get("/size/4");
        assert_eq!(wire.reply().body.len(), 4);
    }
    assert!(rig.client.connect("c.example").is_err());
    rig.wire().get("/size/1");
}

#[test]
fn http_1_1_alpn_and_no_alpn_are_served_by_the_engine() {
    for client in [vec![&b"http/1.1"[..]], vec![]] {
        let rig = serving(vec![b"h2", b"http/1.1"], client);
        let mut wire = rig.wire();
        wire.get("/size/7");
        assert_eq!(wire.reply().body.len(), 7);
        assert!(rig.fixture.handoffs.try_recv().is_err());
    }
}

#[test]
fn an_h2_connection_is_handed_off_with_its_session_before_any_data_is_consumed() {
    let rig = serving(vec![b"h2", b"http/1.1"], vec![b"h2"]);
    let mut wire = rig.wire();
    assert_eq!(wire.stream.conn.alpn_protocol(), Some(&b"h2"[..]));
    wire.send(H2_PREFACE);
    wire.stream.flush().expect("flush");
    let handoff = handed_off(&rig);
    let (mut server, prefix) = session_of(handoff);
    assert!(prefix.is_empty());
    assert_eq!(read_exactly(&mut server, H2_PREFACE.len()), H2_PREFACE);
    server.write_all(b"carried").expect("write");
    server.flush().expect("flush");
    assert_eq!(read_exactly(&mut wire.stream, 7), b"carried");
}

#[test]
fn an_alpn_the_profile_does_not_speak_fails_the_handshake_without_a_handoff() {
    let rig = serving(vec![b"h2", b"http/1.1"], vec![b"unknown"]);
    assert!(rig.client.connect("a.example").is_err());
    assert!(rig.fixture.handoffs.try_recv().is_err());
}

#[test]
fn a_request_the_engine_cannot_serve_is_handed_off_with_its_plaintext_and_session() {
    let rig = serving(vec![b"http/1.1"], vec![b"http/1.1"]);
    let mut wire = rig.wire();
    let request = b"POST /x HTTP/1.1\r\nHost: test.example\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n";
    wire.send(request);
    let handoff = handed_off(&rig);
    let (mut server, prefix) = session_of(handoff);
    assert_eq!(prefix, request);
    server
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
        .expect("answer");
    server.flush().expect("flush");
    assert_eq!(wire.reply().body, b"hi");
}

#[test]
fn a_request_after_served_ones_is_handed_off_with_the_count_served() {
    let rig = serving(vec![b"http/1.1"], vec![b"http/1.1"]);
    let mut wire = rig.wire();
    for _ in 0..2 {
        wire.get("/size/3");
        assert_eq!(wire.reply().body.len(), 3);
    }
    wire.get("/handoff/here");
    let handoff = handed_off(&rig);
    let (mut server, prefix) = session_of(handoff);
    assert_eq!(
        prefix,
        b"GET /handoff/here HTTP/1.1\r\nHost: test.example\r\n\r\n"
    );
    server
        .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
        .expect("answer");
    server.flush().expect("flush");
    assert_eq!(wire.reply().status, 204);
}

#[test]
fn a_required_client_certificate_is_enforced() {
    let client = Identity::new("client.example");
    let rig = Rig::start(
        &Settings {
            client_ca: Some(client.cert.clone()),
            ..Settings::default()
        },
        ClientSettings {
            identity: Some(client),
            ..ClientSettings::default()
        },
    );
    let mut wire = rig.wire();
    wire.get("/size/4");
    assert_eq!(wire.reply().body.len(), 4);
    let anonymous =
        crate::support::Client::new(rig.fixture.addr, &rig.server, ClientSettings::default());
    let refused = anonymous.connect("a.example").and_then(|mut wire| {
        wire.stream
            .write_all(b"GET /size/4 HTTP/1.1\r\nHost: test.example\r\n\r\n")?;
        wire.stream.flush()?;
        read_exactly_io(&mut wire)
    });
    assert!(refused.is_err());
}

fn read_exactly_io(wire: &mut Wire<crate::support::Tls>) -> std::io::Result<()> {
    let mut byte = [0_u8; 1];
    wire.stream.read_exact(&mut byte)
}
