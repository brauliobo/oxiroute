use std::time::Instant as StdInstant;

use super::{
    Arc, Duration, RuntimeGeneration, UdpHarness, UdpSocket, policy, sleep, start_harness, timeout,
};
use crate::clients::{ClientQuery, ClientRow, ConnectionQuery, ConnectionRow, Sort};

fn live_connections(generation: &RuntimeGeneration) -> Vec<ConnectionRow> {
    generation
        .metrics()
        .clients()
        .connections(
            &ConnectionQuery {
                limit: 10,
                ip_prefix: None,
                listener: None,
            },
            StdInstant::now(),
        )
        .connections
}

fn client_rows(generation: &RuntimeGeneration) -> Vec<ClientRow> {
    generation
        .metrics()
        .clients()
        .clients(
            &ClientQuery {
                limit: 10,
                sort: Sort::Bytes,
                ip_prefix: None,
            },
            StdInstant::now(),
        )
        .clients
}

async fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(value) = probe() {
                break value;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("tracked UDP state timeout")
}

async fn exchange(harness: &UdpHarness, client: &UdpSocket, replies: usize) {
    client
        .send_to(b"query", harness.listener)
        .await
        .expect("send query");
    let mut received = [0_u8; 32];
    let (length, peer) = timeout(
        Duration::from_secs(5),
        harness.upstream.recv_from(&mut received),
    )
    .await
    .expect("upstream receive timeout")
    .expect("upstream receive");
    assert_eq!(&received[..length], b"query");
    for _ in 0..replies {
        harness
            .upstream
            .send_to(b"response", peer)
            .await
            .expect("send response");
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_udp_flow_is_tracked_while_open_and_counted_after_it_ends() {
    let harness = start_harness(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let generation = Arc::clone(&harness.generation);
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    exchange(&harness, &client, 1).await;

    let open = wait_for(|| {
        live_connections(&generation)
            .into_iter()
            .find(|row| row.bytes_out == 8)
    })
    .await;
    assert_eq!(open.protocol.as_str(), "udp");
    assert_eq!(&*open.listener, "relay");
    assert_eq!(open.peer, client.local_addr().expect("client address"));
    assert_eq!(open.bytes_in, 5);
    assert_eq!(
        open.upstream.as_deref(),
        Some(harness.upstream.local_addr().expect("upstream").to_string()).as_deref()
    );

    harness.stop();

    assert!(live_connections(&generation).is_empty());
    let rows = client_rows(&generation);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.ip.to_string(), "127.0.0.1");
    assert_eq!(
        (
            row.connections,
            row.connections_total,
            row.requests,
            row.errors
        ),
        (0, 1, 1, 0)
    );
    assert_eq!((row.bytes_in, row.bytes_out), (5, 8));
    assert_eq!(row.protocols, ["udp"]);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_udp_flow_cut_at_its_byte_limit_counts_as_failed() {
    let harness = start_harness(
        policy(),
        Duration::from_secs(10),
        Some(Duration::from_secs(30)),
        8,
        None,
    )
    .await;
    let generation = Arc::clone(&harness.generation);
    let client = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .expect("client socket");
    exchange(&harness, &client, 9).await;

    wait_for(|| {
        live_connections(&generation)
            .is_empty()
            .then_some(())
            .filter(|()| !client_rows(&generation).is_empty())
    })
    .await;
    let rows = client_rows(&generation);
    let row = &rows[0];
    assert_eq!(
        (
            row.connections,
            row.connections_total,
            row.requests,
            row.errors
        ),
        (0, 1, 1, 1)
    );
    assert_eq!((row.bytes_in, row.bytes_out), (5, 56));
    harness.stop();
}
