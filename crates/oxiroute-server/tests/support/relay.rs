#![allow(dead_code, clippy::missing_panics_doc)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpVersionPolicy, L4Service, Listener, Management,
    Protocol, Retirement, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
    time::timeout,
};

use crate::config_support::{empty_config, socket_bind, socket_endpoint};

pub const WIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// A TCP relay listener in front of one upstream pool.
pub fn tcp_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
    retirement: Retirement,
) -> ConfigDraft {
    ConfigDraft {
        retirement: Some(retirement),
        management: Some(Management {
            bind: management_address,
            ui_dir: None,
            token_file: None,
        }),
        listeners: vec![Listener {
            name: "tcp".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        upstream_pools: vec![UpstreamPool {
            name: "upstream".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(upstream_address)],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: Some(10_000),
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        l4_services: vec![L4Service {
            drain_deadline_ms: None,
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 600_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        }],
        ..empty_config()
    }
}

/// An upstream that echoes every byte back.
pub async fn start_echo_upstream() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("echo upstream bind");
    let address = listener.local_addr().expect("echo upstream address");
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.expect("echo upstream accept");
                    connections.spawn(async move {
                        let mut buffer = [0_u8; 1024];
                        loop {
                            let Ok(count) = stream.read(&mut buffer).await else { return };
                            if count == 0 || stream.write_all(&buffer[..count]).await.is_err() {
                                return;
                            }
                        }
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    (address, task)
}

pub async fn assert_echo(stream: &mut TcpStream, payload: &[u8]) {
    stream.write_all(payload).await.expect("relay write");
    let mut response = vec![0_u8; payload.len()];
    timeout(WIRE_TIMEOUT, stream.read_exact(&mut response))
        .await
        .unwrap_or_else(|_| panic!("relay response timeout for {payload:?}"))
        .expect("relay response");
    assert_eq!(response, payload);
}
