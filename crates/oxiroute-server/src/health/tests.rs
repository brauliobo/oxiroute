use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

use super::*;

struct BlockingCheck {
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    release: Semaphore,
    started: AtomicUsize,
}

struct AddressFallbackCheck {
    attempts: StdMutex<Vec<std::net::SocketAddr>>,
    healthy: std::net::SocketAddr,
}

#[async_trait]
impl HealthCheck for AddressFallbackCheck {
    async fn check(&self, target: &Backend) -> pingora::Result<()> {
        let address = *target.addr.as_inet().expect("inet health target");
        self.attempts.lock().expect("health attempts").push(address);
        if address == self.healthy {
            Ok(())
        } else {
            Err(pingora::Error::new_in(ErrorType::ConnectRefused))
        }
    }

    fn health_threshold(&self, _success: bool) -> usize {
        1
    }
}

#[tokio::test]
async fn health_falls_back_to_the_second_resolved_address() {
    let first = std::net::SocketAddr::from(([192, 0, 2, 1], 443));
    let second = std::net::SocketAddr::from(([192, 0, 2, 2], 443));
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "fallback".into(),
            [endpoint.clone()],
            oxiroute_config::UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("health pool"),
    );
    let check = Arc::new(AddressFallbackCheck {
        attempts: StdMutex::new(Vec::new()),
        healthy: second,
    });
    let target = HealthTarget {
        check: Arc::<AddressFallbackCheck>::clone(&check),
        down_interval: Duration::from_secs(1),
        endpoint,
        server: pool.handle(0),
        fast_interval: Duration::from_secs(1),
        healthy_threshold: 1,
        interval: Duration::from_secs(1),
        pool,
        pool_name: "fallback".into(),
        probe_lock: Arc::new(Mutex::new(())),
        timeout: Duration::from_secs(1),
        unhealthy_threshold: 1,
    };

    target
        .run_probe_addresses(&[first, second])
        .await
        .expect("second address is healthy");

    assert_eq!(
        *check.attempts.lock().expect("health attempts"),
        vec![first, second]
    );
}

#[test]
fn health_intervals_follow_checking_down_and_healthy_state() {
    let endpoint = RuntimeEndpoint::from(std::net::SocketAddr::from(([127, 0, 0, 1], 3_000)));
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "intervals".into(),
            [endpoint.clone()],
            oxiroute_config::UpstreamAlgorithm::First,
            true,
        )
        .expect("health pool"),
    );
    let target = HealthTarget {
        check: Arc::new(TcpHealthCheck::default()),
        down_interval: Duration::from_secs(5),
        endpoint,
        server: pool.handle(0),
        fast_interval: Duration::from_secs(1),
        healthy_threshold: 1,
        interval: Duration::from_secs(2),
        pool: Arc::clone(&pool),
        pool_name: "intervals".into(),
        probe_lock: Arc::new(Mutex::new(())),
        timeout: Duration::from_millis(100),
        unhealthy_threshold: 1,
    };

    assert_eq!(target.next_interval(), Duration::from_secs(1));
    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::ConnectFailed),
        Some(1),
        1,
        1,
    );
    assert_eq!(target.next_interval(), Duration::from_secs(5));
    pool.record_health(&pool.handle(0), true, None, Some(2), 1, 1);
    assert_eq!(target.next_interval(), Duration::from_secs(2));
}

#[async_trait]
impl HealthCheck for BlockingCheck {
    async fn check(&self, _target: &Backend) -> pingora::Result<()> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        self.started.fetch_add(1, Ordering::SeqCst);
        self.release
            .acquire()
            .await
            .expect("release semaphore")
            .forget();
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }

    fn health_threshold(&self, _success: bool) -> usize {
        1
    }
}

#[tokio::test]
async fn probes_share_a_global_concurrency_bound() {
    let addresses: Vec<std::net::SocketAddr> = (10_000..10_033)
        .map(|port| ([127, 0, 0, 1], port).into())
        .collect::<Vec<_>>();
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "bounded".into(),
            addresses.iter().copied().map(RuntimeEndpoint::from),
            oxiroute_config::UpstreamAlgorithm::RoundRobin,
            true,
        )
        .expect("health pool"),
    );
    let check = Arc::new(BlockingCheck {
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
        release: Semaphore::new(0),
        started: AtomicUsize::new(0),
    });
    let targets = addresses
        .into_iter()
        .enumerate()
        .map(|(index, address)| HealthTarget {
            check: Arc::<BlockingCheck>::clone(&check),
            down_interval: Duration::from_secs(1),
            endpoint: RuntimeEndpoint::from(address),
            server: pool.handle(index),
            fast_interval: Duration::from_secs(1),
            healthy_threshold: 1,
            interval: Duration::from_secs(1),
            pool: Arc::clone(&pool),
            pool_name: "bounded".into(),
            probe_lock: Arc::new(Mutex::new(())),
            timeout: Duration::from_secs(5),
            unhealthy_threshold: 1,
        })
        .collect();
    let probes = tokio::spawn(run_targets(
        targets,
        Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
    ));

    timeout(Duration::from_secs(1), async {
        while check.started.load(Ordering::SeqCst) < MAX_CONCURRENT_PROBES {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial probes must start");
    sleep(Duration::from_millis(25)).await;
    assert_eq!(check.started.load(Ordering::SeqCst), MAX_CONCURRENT_PROBES);
    assert_eq!(
        check.maximum_active.load(Ordering::SeqCst),
        MAX_CONCURRENT_PROBES
    );

    check.release.add_permits(33);
    probes.await.expect("bounded probes");
    assert_eq!(check.started.load(Ordering::SeqCst), 33);
}

struct CountingCheck {
    calls: AtomicUsize,
}

#[async_trait]
impl HealthCheck for CountingCheck {
    async fn check(&self, _target: &Backend) -> pingora::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn health_threshold(&self, _success: bool) -> usize {
        1
    }
}

fn counting_supervisor(check: &Arc<CountingCheck>) -> (HealthSupervisor, Arc<RoundRobinPool>) {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "dynamic".into(),
            [],
            oxiroute_config::UpstreamAlgorithm::RoundRobin,
            Some(oxiroute_config::HealthStartup::Checking),
            None,
        )
        .expect("empty pool"),
    );
    let template = GroupTemplate {
        check: Arc::<CountingCheck>::clone(check),
        down_interval: Duration::from_millis(10),
        fast_interval: Duration::from_millis(10),
        healthy_threshold: 1,
        interval: Duration::from_millis(10),
        pool: Arc::clone(&pool),
        pool_name: "dynamic".into(),
        timeout: Duration::from_secs(1),
        unhealthy_threshold: 1,
    };
    let group = HealthGroup {
        template,
        targets: Vec::new(),
    };
    (HealthSupervisor::new(vec![group], Vec::new()), pool)
}

fn join(pool: &RoundRobinPool, name: &str, port: u16) -> (ServerHandle, RuntimeEndpoint) {
    let endpoint = RuntimeEndpoint::from(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
    let server = crate::routing::RuntimeServer {
        name: name.into(),
        endpoint: endpoint.clone(),
        max_connections: None,
        pinned_addresses: None,
        protected_addresses: Arc::from([]),
    };
    let spec = crate::routing::DynamicServer {
        weight: 1,
        priority: 100,
    };
    (pool.add_server(server, spec).expect("join"), endpoint)
}

async fn probes_settle(check: &CountingCheck) -> usize {
    sleep(Duration::from_millis(60)).await;
    check.calls.load(Ordering::SeqCst)
}

#[tokio::test]
async fn a_joining_server_is_checked_and_a_leaving_one_is_not() {
    let check = Arc::new(CountingCheck {
        calls: AtomicUsize::new(0),
    });
    let (supervisor, pool) = counting_supervisor(&check);
    let (shutdown, watch) = tokio::sync::watch::channel(false);
    let running = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.run(watch).await }
    });
    assert_eq!(probes_settle(&check).await, 0);

    let (handle, endpoint) = join(&pool, "gpu", 9000);
    supervisor.add_target("dynamic", handle.clone(), endpoint);
    let running_calls = probes_settle(&check).await;
    assert!(running_calls >= 2, "the new server is probed repeatedly");
    assert_eq!(
        pool.health_snapshot().endpoints[0].state,
        crate::EndpointHealthState::Healthy
    );

    supervisor.remove_target("dynamic", &handle);
    let after_removal = probes_settle(&check).await;
    assert_eq!(
        probes_settle(&check).await,
        after_removal,
        "no probes after removal"
    );

    shutdown.send(true).expect("shutdown");
    running.await.expect("supervisor stops");
}

#[tokio::test]
async fn targets_added_before_start_run_once_and_shutdown_cancels_them() {
    let check = Arc::new(CountingCheck {
        calls: AtomicUsize::new(0),
    });
    let (supervisor, pool) = counting_supervisor(&check);
    let (handle, endpoint) = join(&pool, "early", 9001);
    supervisor.add_target("dynamic", handle, endpoint);
    assert_eq!(
        check.calls.load(Ordering::SeqCst),
        0,
        "nothing runs before the supervisor starts"
    );
    let (shutdown, watch) = tokio::sync::watch::channel(false);
    let running = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.run(watch).await }
    });
    assert!(probes_settle(&check).await >= 2);

    shutdown.send(true).expect("shutdown");
    running.await.expect("supervisor stops");
    let stopped = check.calls.load(Ordering::SeqCst);
    assert_eq!(probes_settle(&check).await, stopped);
}

#[tokio::test]
async fn servers_of_a_pool_without_checks_are_ignored() {
    let check = Arc::new(CountingCheck {
        calls: AtomicUsize::new(0),
    });
    let (supervisor, pool) = counting_supervisor(&check);
    let (handle, endpoint) = join(&pool, "other", 9002);

    supervisor.add_target("elsewhere", handle, endpoint);

    assert_eq!(supervisor.state().groups[0].targets.len(), 0);
}

#[test]
fn active_check_transitions_are_reported_as_events() {
    let endpoint = RuntimeEndpoint::from(std::net::SocketAddr::from(([127, 0, 0, 1], 3_001)));
    let pool = RoundRobinPool::new_named(
        "health-events".into(),
        [endpoint],
        oxiroute_config::UpstreamAlgorithm::First,
        true,
    )
    .expect("health pool");
    let before = crate::observability::operational_event::current_cursor();

    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::Timeout),
        Some(1),
        1,
        1,
    );
    pool.record_health(
        &pool.handle(0),
        false,
        Some(HealthFailure::Timeout),
        Some(2),
        1,
        1,
    );
    pool.record_health(&pool.handle(0), true, None, Some(3), 1, 1);

    let outcomes: Vec<_> = crate::observability::operational_event::page(before, 100)
        .events
        .into_iter()
        .filter(|event| event.event.as_str() == "upstream_endpoint_health")
        .map(|event| serde_json::to_value(event.outcome).expect("outcome JSON"))
        .filter(|outcome| outcome["pool"] == "health-events")
        .collect();
    assert_eq!(
        outcomes.len(),
        2,
        "only state changes are events: {outcomes:?}"
    );
    let down = &outcomes[0];
    assert_eq!(
        (
            down["type"].as_str(),
            down["previous"].as_str(),
            down["next"].as_str()
        ),
        (Some("health_changed"), Some("unknown"), Some("unhealthy"))
    );
    assert_eq!(down["reason"], "timeout");
    let up = &outcomes[1];
    assert_eq!(
        (
            up["previous"].as_str(),
            up["next"].as_str(),
            up["reason"].is_null()
        ),
        (Some("unhealthy"), Some("healthy"), true)
    );
}
