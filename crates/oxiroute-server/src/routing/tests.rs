use std::{
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use pingora::{
    connectors::http::Connector as HttpConnector,
    http::RequestHeader,
    protocols::{ConnectionLifetime as _, http::client::HttpSession},
    upstreams::peer::HttpPeer,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

use super::*;

fn runtime_server(name: &str, port: u16, max_connections: Option<u64>) -> RuntimeServer {
    RuntimeServer {
        name: name.into(),
        endpoint: RuntimeEndpoint::from(SocketAddr::from(([127, 0, 0, 1], port))),
        max_connections,
        pinned_addresses: None,
        protected_addresses: Arc::from([]),
    }
}

fn connection_lifetime(pool: &EndpointPool, name: &str) -> Arc<EndpointLeaseInner> {
    let lease = pool
        .select_server_connection_target(name)
        .expect("connection target");
    Arc::clone(&lease.inner)
}

async fn wait_for_lifetime_waiters(pool: &EndpointPool, expected: u64) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.queue.lifetime_waiters.load(Ordering::Relaxed) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lifetime waiter count");
}

fn waiting_pool(wait: Duration, queue_timeout: Option<Duration>) -> Arc<EndpointPool> {
    Arc::new(
        EndpointPool::new_named_servers_resilient(
            "recovering".into(),
            [runtime_server("only", 3000, None)],
            UpstreamAlgorithm::First,
            Some(HealthStartup::Checking),
            queue_timeout,
            PassiveFailurePolicy::default(),
            PoolResiliencePolicy {
                unavailable_wait: Some(wait),
                ..PoolResiliencePolicy::default()
            },
        )
        .expect("pool"),
    )
}

mod admin;
mod balance;
mod dns;
mod evacuate;
mod health;
mod lifetime;
mod passive;
mod queue;
mod reference_selection;
mod resilience;
mod selection;
