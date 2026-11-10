use std::time::Duration;

use pingora::protocols::{Stream, l4::stream::Stream as L4Stream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};

use super::{
    end::{CopyOnly, RelayEnd, Shared},
    splice::{SPLICE_CHUNK, Start, start},
};
use crate::{
    RuntimeMetrics,
    l4::tcp::{RELAY_BUFFER_SIZE, RelayAccounting, RelayDirection, copy_direction},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 31 + index / 7919) % 251).expect("below 251"))
        .collect()
}

async fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let near = TcpStream::connect(listener.local_addr().expect("address"))
        .await
        .expect("connect");
    let (far, _) = listener.accept().await.expect("accept");
    (near, far)
}

fn plain(stream: TcpStream) -> Shared<Stream> {
    Shared::new(Box::new(L4Stream::from(stream)))
}

/// What one direction moved: the bytes the far end received, the bytes that went through the kernel, the counters.
struct Moved {
    received: Vec<u8>,
    spliced: u64,
    counted_received: u64,
    counted_sent: u64,
}

/// Runs one direction between two plain sockets while the feeder writes `chunks` and the collector reads to EOF.
async fn through_sockets(direction: RelayDirection, payload: Vec<u8>, chunk: usize) -> Moved {
    let (mut feeder, source) = socket_pair().await;
    let (sink, mut collector) = socket_pair().await;
    let (source, sink) = (plain(source), plain(sink));
    let feed = tokio::spawn(async move {
        for part in payload.chunks(chunk) {
            feeder.write_all(part).await.expect("feed");
        }
        feeder.shutdown().await.expect("feed EOF");
    });
    let collect = tokio::spawn(async move {
        let mut received = Vec::new();
        collector.read_to_end(&mut received).await.expect("collect");
        received
    });
    let (spliced, counted) = run_direction(direction, &source, &sink).await;
    feed.await.expect("feeder");
    Moved {
        received: collect.await.expect("collector"),
        spliced,
        counted_received: counted.0,
        counted_sent: counted.1,
    }
}

async fn run_direction<S: RelayEnd, T: RelayEnd>(
    direction: RelayDirection,
    source: &Shared<S>,
    sink: &Shared<T>,
) -> (u64, (u64, u64)) {
    let runtime = RuntimeMetrics::new();
    let listener = runtime
        .register_listener("relay-test", "tcp", "127.0.0.1:0", 10)
        .expect("listener");
    let connection = listener.begin_connection().expect("connection");
    let accounting = RelayAccounting::new(&connection, Instant::now());
    let mut buffer = vec![0; RELAY_BUFFER_SIZE];
    let spliced = timeout(
        TEST_TIMEOUT,
        copy_direction(direction, source, sink, &mut buffer, &accounting),
    )
    .await
    .expect("direction timed out")
    .unwrap_or_else(|_| panic!("direction failed"));
    let stats = accounting.stats();
    (spliced, (stats.bytes_received, stats.bytes_sent))
}

#[tokio::test]
async fn a_bulk_payload_between_plain_sockets_goes_through_the_kernel() {
    let payload = pattern(4 * 1024 * 1024 + 123);
    let moved = through_sockets(
        RelayDirection::ClientToUpstream,
        payload.clone(),
        256 * 1024,
    )
    .await;
    assert!(moved.received == payload, "payload differs");
    assert!(moved.spliced > 0, "no byte was spliced");
    assert!(moved.spliced <= payload.len() as u64);
    assert_eq!(moved.counted_received, payload.len() as u64);
    assert_eq!(moved.counted_sent, 0);
}

#[tokio::test]
async fn the_response_direction_counts_the_bytes_it_sends() {
    let payload = pattern(3 * 1024 * 1024 + 7);
    let moved = through_sockets(RelayDirection::UpstreamToClient, payload.clone(), 100_000).await;
    assert!(moved.received == payload, "payload differs");
    assert!(moved.spliced > 0, "no byte was spliced");
    assert_eq!(moved.counted_received, 0);
    assert_eq!(moved.counted_sent, payload.len() as u64);
}

#[tokio::test]
async fn every_write_pattern_arrives_intact_and_counted_the_same() {
    for chunk in [
        1,
        1000,
        RELAY_BUFFER_SIZE,
        RELAY_BUFFER_SIZE + 1,
        SPLICE_CHUNK,
        200_000,
    ] {
        let payload = pattern(1_500_000);
        let moved = through_sockets(RelayDirection::UpstreamToClient, payload.clone(), chunk).await;
        assert!(
            moved.received == payload,
            "payload differs for writes of {chunk}"
        );
        assert_eq!(
            moved.counted_sent,
            payload.len() as u64,
            "writes of {chunk}"
        );
    }
}

#[tokio::test]
async fn small_messages_stay_on_the_copy_loop() {
    let (mut client, source) = socket_pair().await;
    let (sink, mut server) = socket_pair().await;
    let (source, sink) = (plain(source), plain(sink));
    let exchange = tokio::spawn(async move {
        let mut message = [0; 1024];
        for round in 0..64_u8 {
            client.write_all(&[round; 1024]).await.expect("send");
            server.read_exact(&mut message).await.expect("receive");
            assert!(message.iter().all(|byte| *byte == round));
        }
        client.shutdown().await.expect("EOF");
        server.read_to_end(&mut Vec::new()).await.expect("EOF seen");
    });
    let (spliced, (received, _)) =
        run_direction(RelayDirection::ClientToUpstream, &source, &sink).await;
    exchange.await.expect("exchange");
    assert_eq!(spliced, 0);
    assert_eq!(received, 64 * 1024);
}

#[tokio::test]
async fn ends_that_are_not_plain_sockets_are_copied() {
    let payload = pattern(1_000_000);
    let (mut feeder, source) = duplex(64 * 1024);
    let (sink, mut collector) = duplex(64 * 1024);
    let (source, sink) = (Shared::new(CopyOnly(source)), Shared::new(CopyOnly(sink)));
    let expected = payload.clone();
    let feed = tokio::spawn(async move {
        feeder.write_all(&payload).await.expect("feed");
        feeder.shutdown().await.expect("EOF");
    });
    let collect = tokio::spawn(async move {
        let mut received = Vec::new();
        collector.read_to_end(&mut received).await.expect("collect");
        received
    });
    let (spliced, (received, _)) =
        run_direction(RelayDirection::ClientToUpstream, &source, &sink).await;
    feed.await.expect("feeder");
    assert!(
        collect.await.expect("collector") == expected,
        "payload differs"
    );
    assert_eq!(spliced, 0);
    assert_eq!(received, expected.len() as u64);
}

#[tokio::test]
async fn start_is_refused_for_ends_that_are_not_plain_sockets() {
    let (near, _far) = socket_pair().await;
    let (end, _other) = duplex(1024);
    let (source, sink) = (plain(near), Shared::new(CopyOnly(end)));
    assert!(matches!(start(&source, &sink), Start::Never));
}
