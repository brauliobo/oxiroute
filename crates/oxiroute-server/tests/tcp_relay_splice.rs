//! Bulk TCP relays between plain sockets, where the kernel moves the payload: the observable behaviour of the copy
//! relay (bytes, accounting, half close, timeouts, cancellation, failures) must not change.

use std::{net::SocketAddr, time::Duration};

use oxiroute_config::{ProxyProtocolPolicy, ProxyProtocolVersion, UpstreamAlgorithm};
use oxiroute_server::{
    ConnectionGuard, ProxyProtocolTransport, RelayDirection, RelayFailure, RelayFailureKind,
    RelayPolicy, RelayStats, RoundRobinPool, RuntimeEndpoint, RuntimeMetrics, TcpRelayCore,
    accept_stream, encode_header, relay_streams,
};
use pingora::protocols::Stream;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const MIB: usize = 1024 * 1024;

fn pattern(seed: usize, length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 31 + seed + index / 7919) % 251).expect("below 251"))
        .collect()
}

fn pingora_stream(stream: TcpStream) -> Stream {
    Box::new(pingora::protocols::l4::stream::Stream::from(stream))
}

async fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let client = TcpStream::connect(listener.local_addr().expect("address"))
        .await
        .expect("connect");
    let (accepted, _) = listener.accept().await.expect("accept");
    (client, accepted)
}

fn connection_metrics() -> (RuntimeMetrics, ConnectionGuard) {
    let runtime_metrics = RuntimeMetrics::new();
    let listener = runtime_metrics
        .register_listener("tcp-relay", "tcp", "127.0.0.1:0", 100)
        .expect("listener metrics");
    let connection = listener.begin_connection().expect("connection metrics");
    (runtime_metrics, connection)
}

fn policy(idle: Option<Duration>, lifetime: Option<Duration>) -> RelayPolicy {
    RelayPolicy {
        idle,
        lifetime,
        ..RelayPolicy::new(Duration::from_secs(1))
    }
}

type RelayResult = Result<RelayStats, RelayFailure>;

/// A relay between two plain socket pairs: `client` talks to `origin` through it.
struct Relayed {
    client: TcpStream,
    origin: TcpStream,
    relay: JoinHandle<RelayResult>,
    runtime_metrics: RuntimeMetrics,
    shutdown: watch::Sender<bool>,
}

async fn relayed(policy: RelayPolicy) -> Relayed {
    let (client, downstream) = tcp_pair().await;
    let (upstream, origin) = tcp_pair().await;
    let (runtime_metrics, connection) = connection_metrics();
    let (shutdown, watcher) = watch::channel(false);
    let relay = tokio::spawn(async move {
        relay_streams(
            pingora_stream(downstream),
            pingora_stream(upstream),
            &connection,
            watcher,
            policy,
        )
        .await
    });
    Relayed {
        client,
        origin,
        relay,
        runtime_metrics,
        shutdown,
    }
}

async fn read_all(mut stream: TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    stream
        .read_to_end(&mut received)
        .await
        .expect("read to EOF");
    received
}

fn assert_snapshot(runtime_metrics: &RuntimeMetrics, received: usize, sent: usize) {
    let snapshot = runtime_metrics.snapshot().expect("traffic snapshot");
    assert_eq!(snapshot.traffic.bytes_received, received as u64);
    assert_eq!(snapshot.traffic.bytes_sent, sent as u64);
    assert_eq!(snapshot.traffic.active_connections, 0);
}

#[tokio::test]
async fn bulk_data_in_both_directions_arrives_intact_and_is_accounted() {
    timeout(TEST_TIMEOUT, async {
        let (upload, download) = (pattern(1, 6 * MIB + 17), pattern(2, 5 * MIB + 5));
        let Relayed {
            client,
            origin,
            relay,
            runtime_metrics,
            shutdown,
        } = relayed(policy(None, None)).await;
        let (expected_upload, expected_download) = (upload.clone(), download.clone());
        let origin = tokio::spawn(async move {
            let (mut reader, mut writer) = origin.into_split();
            let send = async {
                writer.write_all(&download).await.expect("origin send");
                writer.shutdown().await.expect("origin half-close");
            };
            let receive = async {
                let mut received = Vec::new();
                reader
                    .read_to_end(&mut received)
                    .await
                    .expect("origin receive");
                received
            };
            let ((), received) = tokio::join!(send, receive);
            assert!(received == expected_upload, "upload differs");
        });
        let (mut reader, mut writer) = client.into_split();
        let send = async {
            writer.write_all(&upload).await.expect("client send");
            writer.shutdown().await.expect("client half-close");
        };
        let receive = async {
            let mut received = Vec::new();
            reader
                .read_to_end(&mut received)
                .await
                .expect("client receive");
            received
        };
        let ((), received) = tokio::join!(send, receive);
        assert!(received == expected_download, "download differs");
        origin.await.expect("origin task");
        let stats = relay.await.expect("relay task").expect("relay result");
        assert_eq!(stats.bytes_received, upload.len() as u64);
        assert_eq!(stats.bytes_sent, expected_download.len() as u64);
        assert_snapshot(&runtime_metrics, upload.len(), expected_download.len());
        drop(shutdown);
    })
    .await
    .expect("bulk relay timed out");
}

#[tokio::test]
async fn half_close_after_bulk_data_keeps_the_reverse_direction_open() {
    timeout(TEST_TIMEOUT, async {
        let (request, response) = (pattern(3, 2 * MIB), pattern(4, 3 * MIB));
        let Relayed {
            mut client,
            mut origin,
            relay,
            shutdown: _shutdown,
            ..
        } = relayed(policy(None, None)).await;
        let expected_request = request.clone();
        let origin = tokio::spawn(async move {
            let mut received = vec![0; expected_request.len()];
            origin
                .read_exact(&mut received)
                .await
                .expect("origin request");
            assert!(received == expected_request, "request differs");
            let mut eof = [0];
            assert_eq!(
                origin.read(&mut eof).await.expect("origin EOF"),
                0,
                "client half-close reaches the origin"
            );
            origin.write_all(&response).await.expect("origin response");
            origin.shutdown().await.expect("origin shutdown");
            response
        });
        client.write_all(&request).await.expect("client request");
        client.shutdown().await.expect("client half-close");
        let received = read_all(client).await;
        let response = origin.await.expect("origin task");
        assert!(received == response, "response differs");
        let stats = relay.await.expect("relay task").expect("relay result");
        assert_eq!(
            (stats.bytes_received, stats.bytes_sent),
            (request.len() as u64, response.len() as u64)
        );
    })
    .await
    .expect("half-close relay timed out");
}

#[tokio::test]
async fn a_slow_reader_gets_every_byte_after_the_pipe_filled() {
    timeout(TEST_TIMEOUT, async {
        let download = pattern(5, 24 * MIB);
        let Relayed {
            client,
            mut origin,
            relay,
            runtime_metrics,
            shutdown: _shutdown,
            ..
        } = relayed(policy(None, None)).await;
        let expected = download.clone();
        let origin = tokio::spawn(async move {
            origin.write_all(&expected).await.expect("origin send");
            origin.shutdown().await.expect("origin half-close");
            read_all(origin).await
        });
        sleep(Duration::from_millis(400)).await;
        let (mut reader, mut writer) = client.into_split();
        writer.shutdown().await.expect("client half-close");
        let mut received = Vec::new();
        reader
            .read_to_end(&mut received)
            .await
            .expect("client receive");
        assert!(received == download, "download differs");
        assert!(origin.await.expect("origin task").is_empty());
        let stats = relay.await.expect("relay task").expect("relay result");
        assert_eq!(stats.bytes_sent, download.len() as u64);
        assert_snapshot(&runtime_metrics, 0, download.len());
    })
    .await
    .expect("slow reader relay timed out");
}

#[tokio::test]
async fn silence_after_bulk_data_hits_the_idle_deadline_with_exact_counters() {
    timeout(TEST_TIMEOUT, async {
        let upload = pattern(6, 2 * MIB);
        let Relayed {
            mut client,
            mut origin,
            relay,
            shutdown: _shutdown,
            ..
        } = relayed(policy(Some(Duration::from_millis(400)), None)).await;
        client.write_all(&upload).await.expect("client send");
        let mut received = vec![0; upload.len()];
        origin
            .read_exact(&mut received)
            .await
            .expect("origin receive");
        assert!(received == upload, "upload differs");
        let failure = relay
            .await
            .expect("relay task")
            .expect_err("idle relay must stop");
        assert!(matches!(failure.kind, RelayFailureKind::IdleTimeout(_)));
        assert_eq!(failure.stats.bytes_received, upload.len() as u64);
        assert_eq!(failure.stats.bytes_sent, 0);
    })
    .await
    .expect("idle relay timed out");
}

#[tokio::test]
async fn steady_bulk_traffic_keeps_the_idle_deadline_away_until_the_lifetime_ends() {
    timeout(TEST_TIMEOUT, async {
        let Relayed {
            mut client,
            origin,
            relay,
            shutdown: _shutdown,
            ..
        } = relayed(policy(
            Some(Duration::from_millis(500)),
            Some(Duration::from_millis(1500)),
        ))
        .await;
        let sink = tokio::spawn(async move { read_all(origin).await.len() });
        let block = pattern(7, 256 * 1024);
        let writer = tokio::spawn(async move {
            while client.write_all(&block).await.is_ok() {
                sleep(Duration::from_millis(50)).await;
            }
        });
        let failure = relay
            .await
            .expect("relay task")
            .expect_err("lifetime must stop the relay");
        assert!(
            matches!(failure.kind, RelayFailureKind::LifetimeTimeout(_)),
            "{failure}"
        );
        assert!(failure.stats.bytes_received > MIB as u64);
        writer.abort();
        sink.abort();
    })
    .await
    .expect("lifetime relay timed out");
}

#[tokio::test]
async fn shutdown_cancels_a_bulk_transfer() {
    timeout(TEST_TIMEOUT, async {
        let Relayed {
            mut client,
            origin,
            relay,
            shutdown,
            ..
        } = relayed(policy(None, None)).await;
        let sink = tokio::spawn(async move { read_all(origin).await });
        let block = pattern(8, MIB);
        let writer = tokio::spawn(async move { while client.write_all(&block).await.is_ok() {} });
        sleep(Duration::from_millis(300)).await;
        shutdown.send(true).expect("shutdown signal");
        let failure = relay
            .await
            .expect("relay task")
            .expect_err("shutdown must cancel");
        assert!(matches!(failure.kind, RelayFailureKind::Cancelled));
        assert!(failure.stats.bytes_received > 0);
        writer.await.expect("writer stops once the relay is gone");
        sink.abort();
    })
    .await
    .expect("cancelled relay timed out");
}

#[tokio::test]
async fn a_reset_in_the_middle_of_a_bulk_response_is_an_upstream_read_failure() {
    timeout(TEST_TIMEOUT, async {
        let Relayed {
            client,
            origin,
            relay,
            shutdown: _shutdown,
            ..
        } = relayed(policy(None, None)).await;
        let mut client = client;
        client.write_all(b"never read").await.expect("client send");
        let (mut reader, _writer) = client.into_split();
        let reader = tokio::spawn(async move { reader.read_to_end(&mut Vec::new()).await });
        let mut origin = origin;
        origin
            .write_all(&pattern(9, 3 * MIB))
            .await
            .expect("origin send");
        sleep(Duration::from_millis(300)).await;
        // Closing with bytes nobody read resets the connection.
        drop(origin);
        let failure = relay
            .await
            .expect("relay task")
            .expect_err("a reset must fail the relay");
        assert!(
            matches!(
                failure.kind,
                RelayFailureKind::Io {
                    direction: RelayDirection::UpstreamToClient,
                    ..
                }
            ),
            "{failure}"
        );
        assert!(failure.stats.bytes_sent <= 3 * MIB as u64);
        reader.abort();
    })
    .await
    .expect("reset relay timed out");
}

fn echo_origin(listener: TcpListener) -> JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut received = Vec::new();
        stream
            .read_to_end(&mut received)
            .await
            .expect("origin receive");
        stream.write_all(&received).await.expect("origin echo");
        stream.shutdown().await.expect("origin shutdown");
        received
    })
}

#[tokio::test]
async fn the_connecting_relay_moves_bulk_data_between_plain_sockets() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let origin = echo_origin(listener);
        let (client, downstream) = tcp_pair().await;
        let (runtime_metrics, connection) = connection_metrics();
        let pool = Arc::new(
            RoundRobinPool::from_endpoints(
                [RuntimeEndpoint::from(address)],
                UpstreamAlgorithm::RoundRobin,
            )
            .expect("pool"),
        );
        let relay = TcpRelayCore::new(pool.select().expect("lease"), policy(None, None));
        let (_shutdown, watcher) = watch::channel(false);
        let relay = tokio::spawn(async move {
            relay
                .relay(pingora_stream(downstream), &connection, watcher)
                .await
        });
        let payload = pattern(10, 4 * MIB + 3);
        let (mut reader, mut writer) = client.into_split();
        let (send, received) = tokio::join!(
            async {
                writer.write_all(&payload).await.expect("client send");
                writer.shutdown().await.expect("client half-close");
            },
            async {
                let mut received = Vec::new();
                reader
                    .read_to_end(&mut received)
                    .await
                    .expect("client receive");
                received
            }
        );
        let () = send;
        assert!(received == payload, "echo differs");
        assert_eq!(origin.await.expect("origin task"), payload);
        let stats = relay.await.expect("relay task").expect("relay result");
        assert_eq!(
            (stats.bytes_received, stats.bytes_sent),
            (payload.len() as u64, payload.len() as u64)
        );
        assert_snapshot(&runtime_metrics, payload.len(), payload.len());
        assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 0);
    })
    .await
    .expect("connecting relay timed out");
}

#[tokio::test]
async fn bytes_that_followed_the_proxy_header_stay_in_order_before_spliced_data() {
    timeout(TEST_TIMEOUT, async {
        let (client, downstream) = tcp_pair().await;
        let (upstream, origin) = tcp_pair().await;
        let (_runtime_metrics, connection) = connection_metrics();
        let (_shutdown, mut watcher) = watch::channel(false);
        let policy_v2 = ProxyProtocolPolicy {
            version: ProxyProtocolVersion::V2,
            timeout_ms: 1_000,
        };
        let payload = pattern(11, 3 * MIB + 1);
        let header = encode_header(
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Stream,
            SocketAddr::from(([192, 0, 2, 10], 1234)),
            SocketAddr::from(([192, 0, 2, 20], 4321)),
        )
        .expect("header");
        let origin = tokio::spawn(read_all(origin));
        let (mut reader, mut writer) = client.into_split();
        let upload = payload.clone();
        let send = tokio::spawn(async move {
            let mut first = header;
            first.extend_from_slice(&upload[..500]);
            writer
                .write_all(&first)
                .await
                .expect("header and first bytes");
            writer.write_all(&upload[500..]).await.expect("rest");
            writer.shutdown().await.expect("client half-close");
        });
        let accepted = accept_stream(pingora_stream(downstream), policy_v2, &mut watcher)
            .await
            .expect("accepted");
        let stats = relay_streams(
            accepted.stream,
            pingora_stream(upstream),
            &connection,
            watcher,
            policy(None, None),
        )
        .await
        .expect("relay");
        assert_eq!(stats.bytes_received, payload.len() as u64);
        send.await.expect("client send");
        assert!(
            origin.await.expect("origin task") == payload,
            "payload differs"
        );
        assert_eq!(reader.read(&mut [0]).await.expect("client EOF"), 0);
    })
    .await
    .expect("proxy header relay timed out");
}
