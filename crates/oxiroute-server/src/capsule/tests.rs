use bytes::Bytes;
use tokio::{
    io::duplex,
    net::UdpSocket,
    sync::watch,
    time::{Duration, timeout},
};

use super::*;

#[tokio::test]
async fn datagram_capsule_round_trips_context_zero_payload() {
    let (mut writer, mut reader) = duplex(128);
    let payload = Bytes::from_static(b"quic");
    write_datagram_capsule(&mut writer, &payload)
        .await
        .expect("write capsule");
    let capsule = read_capsule(&mut reader, MAX_CAPSULE_VALUE)
        .await
        .expect("read capsule")
        .expect("capsule");
    assert_eq!(capsule.kind, DATAGRAM_CAPSULE);
    assert_eq!(decode_datagram(&capsule.value), Some(Some(&b"quic"[..])));
}

#[tokio::test]
async fn capsule_reader_handles_fragmented_varints_and_payloads() {
    let (mut writer, mut reader) = duplex(1);
    let writer_task = tokio::spawn(async move {
        for byte in [0, 5, 0, b'p', b'i', b'n', b'g'] {
            writer
                .write_all(&[byte])
                .await
                .expect("fragmented capsule byte");
        }
    });

    let capsule = timeout(
        Duration::from_secs(1),
        read_capsule(&mut reader, MAX_CAPSULE_VALUE),
    )
    .await
    .expect("fragmented capsule read timeout")
    .expect("fragmented capsule read")
    .expect("fragmented capsule");
    writer_task.await.expect("fragmented capsule writer");

    assert_eq!(capsule.kind, DATAGRAM_CAPSULE);
    assert_eq!(decode_datagram(&capsule.value), Some(Some(&b"ping"[..])));
}

#[test]
fn varints_use_quic_prefix_widths() {
    for value in [0, 63, 64, 16_383, 16_384, 1 << 30, (1 << 30) + 1] {
        let mut encoded = Vec::new();
        encode_varint(value, &mut encoded).expect("encode varint");
        assert_eq!(decode_varint(&encoded), Some((value, encoded.len())));
    }
}

#[test]
fn oversized_datagrams_are_rejected() {
    let mut value = vec![0];
    value.resize(MAX_UDP_PAYLOAD + 2, 0);
    assert_eq!(decode_datagram(&value), None);
}

#[tokio::test]
async fn capsule_reader_rejects_values_over_the_buffer_limit() {
    let (mut writer, mut reader) = duplex(16);
    let mut header = Vec::new();
    encode_varint(DATAGRAM_CAPSULE, &mut header).expect("capsule kind");
    encode_varint(
        u64::try_from(MAX_CAPSULE_VALUE + 1).expect("capsule length"),
        &mut header,
    )
    .expect("capsule length encoding");
    writer
        .write_all(&header)
        .await
        .expect("oversized capsule header");

    let Err(error) = read_capsule(&mut reader, MAX_CAPSULE_VALUE).await else {
        panic!("oversized capsule accepted");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn nonzero_contexts_are_ignored_but_empty_context_zero_is_valid() {
    assert_eq!(decode_datagram(&[1, b'x']), Some(None));
    assert_eq!(decode_datagram(&[0]), Some(Some(&[][..])));
}

#[tokio::test]
async fn relay_enforces_idle_timeout_during_blocked_client_write() {
    let target = UdpSocket::bind("127.0.0.1:0").await.expect("target bind");
    let relay_socket = UdpSocket::bind("127.0.0.1:0").await.expect("relay bind");
    relay_socket
        .connect(target.local_addr().expect("target address"))
        .await
        .expect("relay connect");
    let relay_address = relay_socket.local_addr().expect("relay address");
    let (client, relay_stream) = duplex(1);
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let relay_task = tokio::spawn(relay_udp(
        relay_stream,
        relay_socket,
        TunnelLimits {
            idle_timeout: Duration::from_millis(25),
            lifetime_timeout: Duration::from_secs(1),
            ..TunnelLimits::default()
        },
        shutdown,
    ));
    target
        .send_to(b"response", relay_address)
        .await
        .expect("target response");

    let outcome = timeout(Duration::from_secs(1), relay_task)
        .await
        .expect("blocked client write timeout")
        .expect("relay task");
    drop(client);
    assert_eq!(outcome.end, UdpRelayEnd::IdleTimeout);
}

#[tokio::test]
async fn relay_enforces_lifetime_during_blocked_client_write() {
    let target = UdpSocket::bind("127.0.0.1:0").await.expect("target bind");
    let relay_socket = UdpSocket::bind("127.0.0.1:0").await.expect("relay bind");
    relay_socket
        .connect(target.local_addr().expect("target address"))
        .await
        .expect("relay connect");
    let relay_address = relay_socket.local_addr().expect("relay address");
    let (client, relay_stream) = duplex(1);
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let relay_task = tokio::spawn(relay_udp(
        relay_stream,
        relay_socket,
        TunnelLimits {
            idle_timeout: Duration::from_secs(1),
            lifetime_timeout: Duration::from_millis(25),
            ..TunnelLimits::default()
        },
        shutdown,
    ));
    target
        .send_to(b"response", relay_address)
        .await
        .expect("target response");

    let outcome = timeout(Duration::from_secs(1), relay_task)
        .await
        .expect("blocked client write timeout")
        .expect("relay task");
    drop(client);
    assert_eq!(outcome.end, UdpRelayEnd::LifetimeTimeout);
}
