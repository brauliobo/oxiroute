use std::net::SocketAddr;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    sync::watch,
    time::{Duration, timeout},
};

use super::*;

const V4_SOURCE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 1234);
const V4_DESTINATION: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 20)), 443);
const V6_SOURCE: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 1234);
const V6_DESTINATION: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8443);

fn policy(version: ProxyProtocolVersion) -> ProxyProtocolPolicy {
    ProxyProtocolPolicy {
        version,
        timeout_ms: 100,
    }
}

#[test]
fn parses_v1_and_preserves_its_wire_limit() {
    let bytes = b"PROXY TCP4 192.0.2.10 198.51.100.20 1234 443\r\npayload";
    let header = parse_header(
        bytes,
        ProxyProtocolVersion::V1,
        ProxyProtocolTransport::Stream,
    )
    .expect("v1 parse")
    .expect("complete header");
    assert_eq!(header.source, V4_SOURCE);
    assert_eq!(header.destination, V4_DESTINATION);
    assert_eq!(&bytes[header.consumed..], b"payload");
    assert!(header.consumed <= MAX_V1_HEADER_BYTES);
}

#[test]
fn parses_v2_stream_and_datagram_families() {
    for (transport, source, destination) in [
        (ProxyProtocolTransport::Stream, V4_SOURCE, V4_DESTINATION),
        (ProxyProtocolTransport::Datagram, V4_SOURCE, V4_DESTINATION),
        (ProxyProtocolTransport::Stream, V6_SOURCE, V6_DESTINATION),
        (ProxyProtocolTransport::Datagram, V6_SOURCE, V6_DESTINATION),
    ] {
        let encoded = encode_header(ProxyProtocolVersion::V2, transport, source, destination)
            .expect("v2 encode");
        let parsed = parse_header(&encoded, ProxyProtocolVersion::V2, transport)
            .expect("v2 parse")
            .expect("complete header");
        assert_eq!(parsed.version, ProxyProtocolVersion::V2);
        assert_eq!(parsed.source, source);
        assert_eq!(parsed.destination, destination);
        assert_eq!(parsed.consumed, encoded.len());
    }
}

#[test]
fn rejects_local_unix_unknown_and_mismatched_forms() {
    let mut local = encode_header(
        ProxyProtocolVersion::V2,
        ProxyProtocolTransport::Stream,
        V4_SOURCE,
        V4_DESTINATION,
    )
    .expect("header");
    local[12] = 0x20;
    assert_eq!(
        parse_header(
            &local,
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Stream
        )
        .expect_err("LOCAL must fail")
        .kind(),
        ProxyProtocolErrorKind::UnsupportedCommand
    );

    let mut unix = local;
    unix[12] = 0x21;
    unix[13] = 0x31;
    assert_eq!(
        parse_header(
            &unix,
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Stream
        )
        .expect_err("UNIX must fail")
        .kind(),
        ProxyProtocolErrorKind::UnsupportedFamily
    );

    let unknown = b"PROXY UNKNOWN\r\n";
    assert_eq!(
        parse_header(
            unknown,
            ProxyProtocolVersion::V1,
            ProxyProtocolTransport::Stream
        )
        .expect_err("UNKNOWN must fail")
        .kind(),
        ProxyProtocolErrorKind::UnsupportedCommand
    );
    assert_eq!(
        parse_header(
            b"PROXY TCP4 192.0.2.1 198.51.100.1 1 2\r\n",
            ProxyProtocolVersion::V1,
            ProxyProtocolTransport::Datagram
        )
        .expect_err("v1 UDP must fail")
        .kind(),
        ProxyProtocolErrorKind::ProtocolMismatch
    );
}

#[tokio::test]
async fn accepts_overread_and_returns_application_bytes() {
    let (mut client, server) = duplex(256);
    let (shutdown_tx, mut shutdown) = watch::channel(false);
    let task = tokio::spawn(async move {
        Box::pin(accept_stream(
            server,
            policy(ProxyProtocolVersion::V1),
            &mut shutdown,
        ))
        .await
        .expect("accept header")
    });
    client
        .write_all(b"PROXY TCP4 192.0.2.10 198.51.100.20 1234 443\r\nhello")
        .await
        .expect("write header and payload");
    let accepted = timeout(Duration::from_secs(1), task)
        .await
        .expect("accept timeout")
        .expect("accept task");
    let mut payload = Vec::new();
    accepted
        .stream
        .take(5)
        .read_to_end(&mut payload)
        .await
        .expect("read payload");
    assert_eq!(payload, b"hello");
    drop(shutdown_tx);
}

#[tokio::test]
async fn header_read_timeout_is_bounded() {
    let (_client, server) = duplex(16);
    let (_shutdown_tx, mut shutdown) = watch::channel(false);
    let error = Box::pin(accept_stream(
        server,
        policy(ProxyProtocolVersion::V2),
        &mut shutdown,
    ))
    .await
    .err()
    .expect("incomplete header must time out");
    assert_eq!(error.kind(), ProxyProtocolErrorKind::Timeout);
}
