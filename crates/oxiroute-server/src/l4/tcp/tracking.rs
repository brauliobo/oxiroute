use std::{net::SocketAddr, sync::Arc, time::Instant};

use oxiroute_config::UpstreamAlgorithm;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

use super::*;
use crate::{
    RoundRobinPool, RuntimeMetrics,
    clients::{ClientQuery, ConnectionQuery, Sort},
};

struct Fixture {
    metrics: RuntimeMetrics,
    listener: crate::ListenerMetrics,
}

impl Fixture {
    fn new() -> Self {
        let metrics = RuntimeMetrics::new();
        let listener = metrics
            .register_listener("tcp-tracked", "tcp", "127.0.0.1:0", 100)
            .expect("listener metrics");
        Self { metrics, listener }
    }

    fn relay(&self, upstream: SocketAddr, client: &TcpStream) -> TcpRelayCore {
        let pool = Arc::new(
            RoundRobinPool::from_endpoints([upstream.into()], UpstreamAlgorithm::RoundRobin)
                .expect("relay pool"),
        );
        let tracked = TrackedClient::open(&self.listener, client.local_addr().ok());
        TcpRelayCore::new(
            pool.select().expect("relay endpoint"),
            RelayPolicy::new(Duration::from_secs(1)),
        )
        .with_client(tracked)
    }

    fn connections(&self) -> crate::clients::ConnectionsView {
        self.metrics.clients().connections(
            &ConnectionQuery {
                limit: 10,
                ip_prefix: None,
                listener: None,
            },
            Instant::now(),
        )
    }

    fn clients(&self) -> crate::clients::ClientsView {
        self.metrics.clients().clients(
            &ClientQuery {
                limit: 10,
                sort: Sort::Bytes,
                ip_prefix: None,
            },
            Instant::now(),
        )
    }
}

async fn downstream_pair() -> (TcpStream, Stream) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("downstream bind");
    let address = listener.local_addr().expect("downstream address");
    let client = TcpStream::connect(address).await.expect("client connect");
    let (accepted, _) = listener.accept().await.expect("downstream accept");
    let stream: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(accepted));
    (client, stream)
}

#[tokio::test]
async fn a_relayed_connection_is_listed_while_open_and_counted_after_it_closes() {
    let fixture = Fixture::new();
    let upstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream bind");
    let upstream_address = upstream_listener.local_addr().expect("upstream address");
    let upstream = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.expect("upstream accept");
        let mut request = [0; 14];
        stream.read_exact(&mut request).await.expect("request");
        stream
            .write_all(b"server-response")
            .await
            .expect("response");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.expect("client EOF");
    });
    let (mut client, downstream) = downstream_pair().await;
    let relay = fixture.relay(upstream_address, &client);
    let guard = fixture.listener.traffic_accounting();
    let (_shutdown_tx, shutdown) = watch::channel(false);
    let relay_task = tokio::spawn(async move { relay.relay(downstream, &guard, shutdown).await });

    client.write_all(b"client-request").await.expect("request");
    let mut response = [0; 15];
    client.read_exact(&mut response).await.expect("response");

    let open = fixture.connections();
    assert_eq!(open.total, 1);
    let row = &open.connections[0];
    assert_eq!(row.protocol, crate::clients::Protocol::Tcp);
    assert_eq!(row.listener.as_ref(), "tcp-tracked");
    assert_eq!(row.bytes_in, 14);
    assert_eq!(
        row.upstream.as_deref(),
        Some(upstream_address.to_string().as_str())
    );

    client.shutdown().await.expect("client half-close");
    upstream.await.expect("upstream task");
    relay_task.await.expect("relay task").expect("relay");

    assert_eq!(fixture.connections().total, 0);
    let view = fixture.clients();
    let client_row = &view.clients[0];
    assert_eq!(client_row.ip.to_string(), "127.0.0.1");
    assert_eq!(
        (client_row.connections, client_row.connections_total),
        (0, 1)
    );
    assert_eq!((client_row.bytes_in, client_row.bytes_out), (14, 15));
    assert_eq!((client_row.requests, client_row.errors), (1, 0));
    assert_eq!(client_row.protocols, ["tcp"]);
}

#[tokio::test]
async fn a_relay_that_cannot_reach_its_upstream_counts_as_a_failed_request() {
    let fixture = Fixture::new();
    let (client, downstream) = downstream_pair().await;
    let relay = fixture.relay(crate::test_ports::unused_tcp_address(), &client);
    let guard = fixture.listener.traffic_accounting();
    let (_shutdown_tx, shutdown) = watch::channel(false);

    relay
        .relay(downstream, &guard, shutdown)
        .await
        .expect_err("the upstream is down");

    assert_eq!(fixture.connections().total, 0);
    let view = fixture.clients();
    let row = &view.clients[0];
    assert_eq!((row.requests, row.errors), (1, 1));
    assert_eq!((row.bytes_in, row.bytes_out), (0, 0));
}
