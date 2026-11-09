use tokio::{
    io::{BufWriter, duplex},
    time::timeout,
};

use super::*;

#[tokio::test]
async fn overread_bytes_are_delivered_before_the_socket() {
    let (mut peer, stream) = duplex(64);
    peer.write_all(b"socket").await.unwrap();
    let mut stream = OverreadIo::new(stream, Bytes::from_static(b"prefix-"));
    let mut bytes = vec![0; 13];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"prefix-socket");
}

#[tokio::test]
async fn tunnel_stops_exactly_at_direction_limit() {
    let (mut client, proxy_left) = duplex(64);
    let (proxy_right, mut server) = duplex(64);
    let proxy_right = BufWriter::with_capacity(64, proxy_right);
    let tunnel = BoundedTunnel::new(TunnelLimits {
        max_bytes_per_direction: 4,
        idle_timeout: Duration::from_secs(1),
        lifetime_timeout: Duration::from_secs(2),
        buffer_size: 16,
    })
    .unwrap();
    let relay = tokio::spawn(async move { tunnel.relay(proxy_left, proxy_right).await });
    client.write_all(b"1234").await.unwrap();
    let mut received = [0; 4];
    server.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"1234");
    assert!(matches!(
        timeout(Duration::from_secs(30), relay)
            .await
            .unwrap()
            .unwrap(),
        TunnelOutcome::Ended {
            end: TunnelEnd::ByteLimitLeftToRight,
            stats: TunnelStats {
                left_to_right: 4,
                ..
            }
        }
    ));
}

#[tokio::test]
async fn tunnel_relays_both_directions_and_half_closes() {
    let (mut client, proxy_left) = duplex(64);
    let (proxy_right, mut server) = duplex(64);
    let tunnel = BoundedTunnel::new(TunnelLimits {
        max_bytes_per_direction: 64,
        idle_timeout: Duration::from_secs(1),
        lifetime_timeout: Duration::from_secs(2),
        buffer_size: 16,
    })
    .unwrap();
    let relay = tokio::spawn(async move { tunnel.relay(proxy_left, proxy_right).await });

    client.write_all(b"request").await.unwrap();
    server.write_all(b"response").await.unwrap();
    let mut request = [0; 7];
    let mut response = [0; 8];
    server.read_exact(&mut request).await.unwrap();
    client.read_exact(&mut response).await.unwrap();
    assert_eq!(&request, b"request");
    assert_eq!(&response, b"response");
    client.shutdown().await.unwrap();
    server.shutdown().await.unwrap();

    assert!(matches!(
        relay.await.unwrap(),
        TunnelOutcome::Ended {
            end: TunnelEnd::Eof,
            stats: TunnelStats {
                left_to_right: 7,
                right_to_left: 8,
            },
        }
    ));
}

#[tokio::test]
async fn tunnel_flushes_each_direction_when_the_peer_awaits_more_input() {
    let (mut client, proxy_left) = duplex(64);
    let (proxy_right, mut server) = duplex(64);
    let tunnel = BoundedTunnel::new(TunnelLimits {
        max_bytes_per_direction: 64,
        idle_timeout: Duration::from_secs(1),
        lifetime_timeout: Duration::from_secs(2),
        buffer_size: 16,
    })
    .unwrap();
    let relay =
        tokio::spawn(async move { tunnel.relay(proxy_left, BufWriter::new(proxy_right)).await });

    client.write_all(b"request").await.unwrap();
    let mut request = [0; 7];
    timeout(Duration::from_secs(30), server.read_exact(&mut request))
        .await
        .expect("buffered tunnel flush")
        .unwrap();
    assert_eq!(&request, b"request");
    client.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
    assert!(matches!(
        relay.await.unwrap(),
        TunnelOutcome::Ended {
            end: TunnelEnd::Eof,
            ..
        }
    ));
}

#[test]
fn tunnel_outcomes_have_stable_safe_labels() {
    let ended = TunnelOutcome::Ended {
        end: TunnelEnd::IdleTimeout,
        stats: TunnelStats::default(),
    };
    let failed = TunnelOutcome::Io {
        stats: TunnelStats::default(),
        source: io::Error::other("not logged"),
    };

    assert_eq!(ended.kind(), TunnelOutcomeKind::IdleTimeout);
    assert_eq!(ended.kind().as_str(), "idle_timeout");
    assert_eq!(failed.kind(), TunnelOutcomeKind::IoError);
    assert_eq!(failed.stats(), TunnelStats::default());
}
