use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use oxiroute_server::{
    ConnectionGuard, RelayDirection, RelayFailureKind, RelayOperation, RelayPolicy, RuntimeMetrics,
    TcpRelayCore, relay_streams,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf, duplex},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    time::{sleep, timeout},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn relays_bidirectional_traffic_and_accounts_for_it() {
    timeout(TEST_TIMEOUT, async {
        let upstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut request = [0; 14];
            stream
                .read_exact(&mut request)
                .await
                .expect("upstream request");
            assert_eq!(&request, b"client-request");
            stream
                .write_all(b"server-response")
                .await
                .expect("upstream response");
            stream.shutdown().await.expect("upstream half-close");
        });

        let downstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("downstream bind");
        let downstream_address = downstream_listener
            .local_addr()
            .expect("downstream address");
        let mut client = TcpStream::connect(downstream_address)
            .await
            .expect("client connect");
        let (downstream, _) = downstream_listener
            .accept()
            .await
            .expect("downstream accept");
        let (runtime_metrics, connection) = connection_metrics();
        let relay = TcpRelayCore::new(upstream_address, policy(None, None));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let relay_task = tokio::spawn(async move {
            relay
                .relay(pingora_stream(downstream), &connection, shutdown)
                .await
        });

        client
            .write_all(b"client-request")
            .await
            .expect("client request");
        let mut response = [0; 15];
        client
            .read_exact(&mut response)
            .await
            .expect("client response");
        assert_eq!(&response, b"server-response");
        client.shutdown().await.expect("client half-close");
        let mut remainder = Vec::new();
        client
            .read_to_end(&mut remainder)
            .await
            .expect("client EOF");
        assert!(remainder.is_empty());

        upstream.await.expect("upstream task");
        let stats = relay_task
            .await
            .expect("relay task")
            .expect("successful relay");
        drop(shutdown_tx);
        assert_eq!(stats.bytes_received, 14);
        assert_eq!(stats.bytes_sent, 15);
        let snapshot = runtime_metrics.snapshot().expect("traffic snapshot");
        assert_eq!(snapshot.traffic.bytes_received, 14);
        assert_eq!(snapshot.traffic.bytes_sent, 15);
        assert_eq!(snapshot.traffic.active_connections, 0);
    })
    .await
    .expect("bidirectional relay timed out");
}

#[tokio::test]
async fn client_half_close_preserves_the_reverse_response_path() {
    timeout(TEST_TIMEOUT, async {
        let upstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut request = Vec::new();
            stream
                .read_to_end(&mut request)
                .await
                .expect("request through half-close");
            assert_eq!(request, b"request-before-eof");
            stream
                .write_all(b"response-after-eof")
                .await
                .expect("reverse response");
            stream.shutdown().await.expect("upstream half-close");
        });

        let downstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("downstream bind");
        let downstream_address = downstream_listener
            .local_addr()
            .expect("downstream address");
        let mut client = TcpStream::connect(downstream_address)
            .await
            .expect("client connect");
        let (downstream, _) = downstream_listener
            .accept()
            .await
            .expect("downstream accept");
        let (_runtime_metrics, connection) = connection_metrics();
        let relay = TcpRelayCore::new(upstream_address, policy(None, None));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let relay_task = tokio::spawn(async move {
            relay
                .relay(pingora_stream(downstream), &connection, shutdown)
                .await
        });

        client
            .write_all(b"request-before-eof")
            .await
            .expect("client request");
        client.shutdown().await.expect("client half-close");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("response after client EOF");
        assert_eq!(response, b"response-after-eof");

        upstream.await.expect("upstream task");
        let stats = relay_task
            .await
            .expect("relay task")
            .expect("successful relay");
        drop(shutdown_tx);
        assert_eq!(stats.bytes_received, 18);
        assert_eq!(stats.bytes_sent, 18);
    })
    .await
    .expect("half-close relay timed out");
}

#[tokio::test]
async fn stops_an_idle_connection_at_the_idle_deadline() {
    timeout(TEST_TIMEOUT, async {
        let upstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.expect("upstream close"), 0);
        });
        let (client, downstream) = downstream_pair().await;
        let (_runtime_metrics, connection) = connection_metrics();
        let idle_timeout = Duration::from_millis(50);
        let relay = TcpRelayCore::new(upstream_address, policy(Some(idle_timeout), None));
        let (shutdown_tx, shutdown) = watch::channel(false);

        let failure = relay
            .relay(pingora_stream(downstream), &connection, shutdown)
            .await
            .expect_err("idle relay must time out");
        assert!(matches!(
            failure.kind,
            RelayFailureKind::IdleTimeout(duration) if duration == idle_timeout
        ));
        assert_eq!(failure.stats.bytes_received, 0);
        assert_eq!(failure.stats.bytes_sent, 0);

        drop(client);
        drop(shutdown_tx);
        upstream.await.expect("upstream task");
    })
    .await
    .expect("idle-timeout test timed out");
}

#[tokio::test]
async fn lifetime_timeout_stops_an_active_connection() {
    timeout(TEST_TIMEOUT, async {
        let upstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.expect("upstream accept");
            loop {
                if stream.write_all(b"x").await.is_err() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        });
        let (client, downstream) = downstream_pair().await;
        let (runtime_metrics, connection) = connection_metrics();
        let idle_timeout = Duration::from_millis(40);
        let lifetime_timeout = Duration::from_millis(150);
        let relay = TcpRelayCore::new(
            upstream_address,
            policy(Some(idle_timeout), Some(lifetime_timeout)),
        );
        let (shutdown_tx, shutdown) = watch::channel(false);

        let failure = relay
            .relay(pingora_stream(downstream), &connection, shutdown)
            .await
            .expect_err("active relay must reach its lifetime");
        assert!(matches!(
            failure.kind,
            RelayFailureKind::LifetimeTimeout(duration) if duration == lifetime_timeout
        ));
        assert!(failure.stats.bytes_sent > 0);
        let snapshot = runtime_metrics.snapshot().expect("traffic snapshot");
        assert_eq!(snapshot.traffic.bytes_sent, failure.stats.bytes_sent);

        drop(client);
        drop(shutdown_tx);
        upstream.await.expect("upstream task");
    })
    .await
    .expect("lifetime-timeout test timed out");
}

#[tokio::test]
async fn shutdown_signal_cancels_an_established_relay() {
    timeout(TEST_TIMEOUT, async {
        let upstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.expect("upstream accept");
            accepted_tx.send(()).expect("accepted notification");
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.expect("upstream close"), 0);
        });
        let (client, downstream) = downstream_pair().await;
        let (_runtime_metrics, connection) = connection_metrics();
        let relay = TcpRelayCore::new(upstream_address, policy(None, None));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let cancellation = tokio::spawn(async move {
            accepted_rx.await.expect("upstream acceptance");
            shutdown_tx.send(true).expect("shutdown signal");
        });

        let failure = relay
            .relay(pingora_stream(downstream), &connection, shutdown)
            .await
            .expect_err("shutdown must cancel relay");
        assert!(matches!(failure.kind, RelayFailureKind::Cancelled));

        drop(client);
        cancellation.await.expect("cancellation task");
        upstream.await.expect("upstream task");
    })
    .await
    .expect("shutdown-cancellation test timed out");
}

#[tokio::test]
async fn accounts_for_partial_downstream_writes_before_an_error() {
    timeout(TEST_TIMEOUT, async {
        let accepted = Arc::new(AtomicUsize::new(0));
        let downstream = PartialWriteStream {
            accepted: Arc::clone(&accepted),
            write_limit: 3,
        };
        let (relay_upstream, mut upstream_peer) = duplex(64);
        upstream_peer
            .write_all(b"partial-response")
            .await
            .expect("upstream payload");
        upstream_peer.shutdown().await.expect("upstream half-close");
        let (runtime_metrics, connection) = connection_metrics();
        let (shutdown_tx, shutdown) = watch::channel(false);

        let failure = relay_streams(
            downstream,
            relay_upstream,
            &connection,
            shutdown,
            policy(None, None),
        )
        .await
        .expect_err("scripted downstream write must fail");
        assert!(matches!(
            &failure.kind,
            RelayFailureKind::Io {
                direction: RelayDirection::UpstreamToClient,
                operation: RelayOperation::Write,
                ..
            }
        ));
        assert_eq!(failure.stats.bytes_sent, 3);
        assert_eq!(accepted.load(Ordering::Relaxed), 3);
        let snapshot = runtime_metrics.snapshot().expect("traffic snapshot");
        assert_eq!(snapshot.traffic.bytes_sent, 3);

        drop(shutdown_tx);
    })
    .await
    .expect("partial-accounting test timed out");
}

fn connection_metrics() -> (RuntimeMetrics, ConnectionGuard) {
    let runtime_metrics = RuntimeMetrics::new();
    let listener = runtime_metrics
        .register_listener("tcp-relay", "tcp", "127.0.0.1:0", 100)
        .expect("listener metrics");
    let connection = listener.begin_connection().expect("connection metrics");
    (runtime_metrics, connection)
}

async fn downstream_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("downstream bind");
    let address = listener.local_addr().expect("downstream address");
    let client = TcpStream::connect(address).await.expect("client connect");
    let (downstream, _) = listener.accept().await.expect("downstream accept");
    (client, downstream)
}

fn pingora_stream(stream: TcpStream) -> pingora::protocols::Stream {
    Box::new(pingora::protocols::l4::stream::Stream::from(stream))
}

fn policy(idle_timeout: Option<Duration>, lifetime_timeout: Option<Duration>) -> RelayPolicy {
    RelayPolicy {
        idle: idle_timeout,
        lifetime: lifetime_timeout,
        ..RelayPolicy::new(Duration::from_secs(1))
    }
}

struct PartialWriteStream {
    accepted: Arc<AtomicUsize>,
    write_limit: usize,
}

impl AsyncRead for PartialWriteStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for PartialWriteStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let accepted = self.accepted.load(Ordering::Relaxed);
        if accepted >= self.write_limit {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "scripted downstream write failure",
            )));
        }
        let written = buffer.len().min(self.write_limit - accepted);
        self.accepted.fetch_add(written, Ordering::Relaxed);
        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
