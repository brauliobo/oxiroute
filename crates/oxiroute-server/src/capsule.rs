use std::io;

use oxiroute_forward_proxy::TunnelLimits;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, split},
    net::UdpSocket,
    sync::{mpsc, watch},
    time::{Instant, sleep_until},
};

const DATAGRAM_CAPSULE: u64 = 0;
const UDP_CONTEXT_ID: u64 = 0;
pub(crate) const MAX_UDP_PAYLOAD: usize = 65_527;
const MAX_CAPSULE_VALUE: usize = MAX_UDP_PAYLOAD + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UdpRelayEnd {
    Eof,
    ByteLimitClientToUdp,
    ByteLimitUdpToClient,
    IdleTimeout,
    LifetimeTimeout,
    Cancelled,
    IoError,
    Malformed,
}

impl UdpRelayEnd {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::ByteLimitClientToUdp => "byte_limit_client_to_udp",
            Self::ByteLimitUdpToClient => "byte_limit_udp_to_client",
            Self::IdleTimeout => "idle_timeout",
            Self::LifetimeTimeout => "lifetime_timeout",
            Self::Cancelled => "cancelled",
            Self::IoError => "io_error",
            Self::Malformed => "malformed",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct UdpRelayStats {
    pub(crate) client_to_udp: u64,
    pub(crate) udp_to_client: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UdpRelayOutcome {
    pub(crate) end: UdpRelayEnd,
    pub(crate) stats: UdpRelayStats,
}

pub(crate) async fn relay_udp<S>(
    stream: S,
    socket: UdpSocket,
    limits: TunnelLimits,
    mut shutdown: watch::Receiver<bool>,
) -> UdpRelayOutcome
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let deadline = Instant::now() + limits.lifetime_timeout;
    let mut idle = Box::pin(sleep_until(Instant::now() + limits.idle_timeout));
    let lifetime = sleep_until(deadline);
    tokio::pin!(lifetime);
    let (mut reader, mut writer) = split(stream);
    let (capsule_sender, mut capsule_receiver) = mpsc::channel(1);
    let reader_task = tokio::spawn(async move {
        loop {
            let result = read_capsule(&mut reader, MAX_CAPSULE_VALUE).await;
            let terminal = matches!(&result, Ok(None) | Err(_));
            if capsule_sender.send(result).await.is_err() || terminal {
                break;
            }
        }
    });
    let mut udp_buffer = [0; MAX_UDP_PAYLOAD];
    let mut stats = UdpRelayStats::default();

    let end = loop {
        let event = {
            let receive = socket.recv(&mut udp_buffer);
            let capsule = capsule_receiver.recv();
            tokio::pin!(receive);
            tokio::pin!(capsule);
            tokio::select! {
                biased;
                _ = shutdown.changed() => break UdpRelayEnd::Cancelled,
                _ = &mut lifetime => break UdpRelayEnd::LifetimeTimeout,
                _ = &mut idle => break UdpRelayEnd::IdleTimeout,
                result = &mut capsule => RelayEvent::Capsule(result),
                result = &mut receive => RelayEvent::Udp(result),
            }
        };
        match event {
            RelayEvent::Capsule(result) => {
                let received = match result {
                    Some(result) => match result {
                        Ok(Some(capsule)) => capsule,
                        Ok(None) => break UdpRelayEnd::Eof,
                        Err(error) => {
                            log::debug!("HTTP/1 CONNECT-UDP capsule read failed: {error}");
                            break if error.kind() == io::ErrorKind::InvalidData {
                                UdpRelayEnd::Malformed
                            } else {
                                UdpRelayEnd::IoError
                            };
                        }
                    },
                    None => break UdpRelayEnd::IoError,
                };
                idle.as_mut().reset(Instant::now() + limits.idle_timeout);
                if received.kind != DATAGRAM_CAPSULE {
                    continue;
                }
                let Some(payload) = decode_datagram(&received.value) else {
                    break UdpRelayEnd::Malformed;
                };
                let Some(payload) = payload else {
                    continue;
                };
                let next = stats
                    .client_to_udp
                    .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
                if next > limits.max_bytes_per_direction {
                    break UdpRelayEnd::ByteLimitClientToUdp;
                }
                let result = tokio::select! {
                    biased;
                    _ = shutdown.changed() => break UdpRelayEnd::Cancelled,
                    _ = &mut lifetime => break UdpRelayEnd::LifetimeTimeout,
                    _ = &mut idle => break UdpRelayEnd::IdleTimeout,
                    result = socket.send(payload) => result,
                };
                match result {
                    Ok(length) if length == payload.len() => {}
                    Ok(_) | Err(_) => break UdpRelayEnd::IoError,
                }
                stats.client_to_udp = next;
            }
            RelayEvent::Udp(result) => {
                let length = match result {
                    Ok(length) => length,
                    Err(error) => {
                        log::debug!("HTTP/1 CONNECT-UDP UDP read failed: {error}");
                        break UdpRelayEnd::IoError;
                    }
                };
                let next = stats
                    .udp_to_client
                    .saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
                if next > limits.max_bytes_per_direction {
                    break UdpRelayEnd::ByteLimitUdpToClient;
                }
                let result = tokio::select! {
                    biased;
                    _ = shutdown.changed() => break UdpRelayEnd::Cancelled,
                    _ = &mut lifetime => break UdpRelayEnd::LifetimeTimeout,
                    _ = &mut idle => break UdpRelayEnd::IdleTimeout,
                    result = write_datagram_capsule(&mut writer, &udp_buffer[..length]) => result,
                };
                match result {
                    Ok(()) => {}
                    Err(_) => break UdpRelayEnd::IoError,
                }
                stats.udp_to_client = next;
                idle.as_mut().reset(Instant::now() + limits.idle_timeout);
            }
        }
    };
    reader_task.abort();
    let _ = reader_task.await;

    UdpRelayOutcome { end, stats }
}

struct Capsule {
    kind: u64,
    value: Vec<u8>,
}

enum RelayEvent {
    Capsule(Option<io::Result<Option<Capsule>>>),
    Udp(io::Result<usize>),
}

async fn read_capsule<R>(reader: &mut R, max_value: usize) -> io::Result<Option<Capsule>>
where
    R: AsyncRead + Unpin,
{
    let Some(kind) = read_varint(reader, true).await? else {
        return Ok(None);
    };
    let length = read_varint(reader, false)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated capsule length"))?;
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length <= max_value)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "capsule is too large"))?;
    let mut value = vec![0; length];
    reader.read_exact(&mut value).await?;
    Ok(Some(Capsule { kind, value }))
}

async fn read_varint<R>(reader: &mut R, allow_eof: bool) -> io::Result<Option<u64>>
where
    R: AsyncRead + Unpin,
{
    let first = match reader.read_u8().await {
        Ok(first) => first,
        Err(error) if allow_eof && error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    let width = 1_usize << usize::from(first >> 6);
    let mut value = u64::from(first & 0x3f);
    for _ in 1..width {
        value = (value << 8) | u64::from(reader.read_u8().await?);
    }
    Ok(Some(value))
}

fn decode_datagram(value: &[u8]) -> Option<Option<&[u8]>> {
    let (context, consumed) = decode_varint(value)?;
    if context != UDP_CONTEXT_ID {
        return Some(None);
    }
    let payload = &value[consumed..];
    (payload.len() <= MAX_UDP_PAYLOAD).then_some(Some(payload))
}

fn decode_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let width = 1_usize << usize::from(first >> 6);
    if bytes.len() < width {
        return None;
    }
    let mut value = u64::from(first & 0x3f);
    for byte in &bytes[1..width] {
        value = (value << 8) | u64::from(*byte);
    }
    Some((value, width))
}

async fn write_datagram_capsule<W>(writer: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let value_length = payload
        .len()
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP payload is too large"))?;
    let mut capsule = Vec::with_capacity(payload.len() + 16);
    encode_varint(DATAGRAM_CAPSULE, &mut capsule)?;
    encode_varint(
        u64::try_from(value_length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "capsule length overflows varint",
            )
        })?,
        &mut capsule,
    )?;
    encode_varint(UDP_CONTEXT_ID, &mut capsule)?;
    capsule.extend_from_slice(payload);
    writer.write_all(&capsule).await
}

fn encode_varint(value: u64, output: &mut Vec<u8>) -> io::Result<()> {
    if value > 0x3fff_ffff_ffff_ffff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "value does not fit in a QUIC varint",
        ));
    }
    let (width, prefix) = if value <= 0x3f {
        (1, 0)
    } else if value <= 0x3fff {
        (2, 0x40)
    } else if value <= 0x3fff_ffff {
        (4, 0x80)
    } else {
        (8, 0xc0)
    };
    let shift = (width - 1) * 8;
    output.push((prefix | ((value >> shift) as u8)) as u8);
    for index in (0..shift).step_by(8).rev() {
        output.push((value >> index) as u8);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tokio::{
        io::{AsyncWriteExt as _, duplex},
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

        let error = match read_capsule(&mut reader, MAX_CAPSULE_VALUE).await {
            Ok(_) => panic!("oversized capsule accepted"),
            Err(error) => error,
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
}
