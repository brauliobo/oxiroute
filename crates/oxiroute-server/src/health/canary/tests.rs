use std::{net::SocketAddr, time::Duration};

use oxiroute_config::{HealthStartup, PoolCanary, UpstreamAlgorithm};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

use super::*;
use crate::test_ports::unused_tcp_address;
use crate::{AdministrativeState, HealthSupervisor, routing::RuntimeServer};

/// Answers every request with `status` after `delay`; returns its address.
async fn origin(status: u16, delay: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer).await;
                tokio::time::sleep(delay).await;
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    address
}

fn canary(max_latency_ms: Option<u64>) -> PoolCanary {
    PoolCanary {
        path: "/healthz/deep".into(),
        host: Some("farm.example.com".into()),
        expected_status: 200,
        interval_ms: 1_000,
        timeout_ms: 1_000,
        max_latency_ms,
    }
}

fn runner(address: SocketAddr, max_connections: Option<u64>, canary: &PoolCanary) -> CanaryRunner {
    let server = RuntimeServer {
        name: "a".into(),
        endpoint: address.into(),
        max_connections,
        pinned_addresses: None,
        protected_addresses: Arc::from([]),
    };
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "farm".into(),
            [server],
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool"),
    );
    CanaryRunner::new(pool, &CanaryBlueprint::compile(canary).expect("canary"))
}

#[tokio::test]
async fn a_correct_answer_is_ok_and_records_the_server_and_time() {
    let runner = runner(origin(200, Duration::ZERO).await, None, &canary(None));
    runner.run_once().await;
    let snapshot = runner.pool.health_snapshot().canary.expect("canary state");
    assert_eq!(snapshot.result, Some(CanaryResult::Ok));
    assert_eq!(snapshot.server.as_deref(), Some("a"));
    assert!(snapshot.last_success_unix_ms.is_some());
}

#[tokio::test]
async fn a_wrong_status_fails_with_the_reason_and_counts() {
    let runner = runner(origin(503, Duration::ZERO).await, None, &canary(None));
    runner.run_once().await;
    runner.run_once().await;
    let snapshot = runner.pool.health_snapshot().canary.expect("canary state");
    assert_eq!(snapshot.result, Some(CanaryResult::Failed));
    assert_eq!(snapshot.reason, Some(HealthFailure::UnexpectedStatus));
    assert_eq!(snapshot.consecutive_failures, 2);
}

#[tokio::test]
async fn a_refused_connection_fails_and_a_slow_answer_is_slow() {
    let address = unused_tcp_address();
    let refused = runner(address, None, &canary(None));
    refused.run_once().await;
    let snapshot = refused.pool.health_snapshot().canary.expect("canary state");
    assert_eq!(snapshot.reason, Some(HealthFailure::ConnectFailed));

    let slow = runner(
        origin(200, Duration::from_millis(150)).await,
        None,
        &canary(Some(50)),
    );
    slow.run_once().await;
    let snapshot = slow.pool.health_snapshot().canary.expect("canary state");
    assert_eq!(snapshot.result, Some(CanaryResult::Slow));
    assert!(snapshot.duration_ms >= 100);
}

#[tokio::test]
async fn a_saturated_pool_is_skipped_and_a_drained_pool_is_unavailable() {
    let runner = runner(origin(200, Duration::ZERO).await, Some(1), &canary(None));
    let held = runner.pool.select().expect("a client holds the only slot");
    runner.run_once().await;
    let snapshot = runner.pool.health_snapshot().canary.expect("canary state");
    assert_eq!((snapshot.result, snapshot.runs["busy"]), (None, 1));
    drop(held);

    runner
        .pool
        .set_administrative_state(AdministrativeState::Drain);
    runner.run_once().await;
    let snapshot = runner.pool.health_snapshot().canary.expect("canary state");
    assert_eq!(snapshot.result, Some(CanaryResult::Unavailable));
    assert_eq!(snapshot.consecutive_failures, 0);
}

#[tokio::test]
async fn the_supervisor_probes_canaries_with_the_health_checks() {
    let runner = runner(origin(200, Duration::ZERO).await, None, &canary(None));
    let pool = Arc::clone(&runner.pool);
    let supervisor = HealthSupervisor::new(Vec::new(), vec![runner]);
    assert!(!supervisor.is_empty());
    supervisor.probe_once().await;
    assert_eq!(
        pool.health_snapshot().canary.expect("canary state").result,
        Some(CanaryResult::Ok)
    );
}
