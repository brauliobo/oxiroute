#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
async fn reusable_path_waiters_are_counted_in_the_pool_queue() {
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "bounded".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(1)),
        )
        .expect("bounded pool"),
    );
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    assert!(!second.try_acquire().expect("saturated acquisition"));
    let generation = second.capacity_generation();
    let waiting = Arc::clone(&second);
    let waiter = tokio::spawn(async move { waiting.wait_for_capacity(generation).await });
    wait_for_lifetime_waiters(&pool, 1).await;

    let waiting_snapshot = pool.health_snapshot();
    assert_eq!(waiting_snapshot.queued, 1);
    assert_eq!(waiting_snapshot.queued_total, 1);

    drop(first);
    waiter.await.expect("waiter task").expect("capacity wake");
    assert!(second.try_acquire().expect("woken acquisition"));
    let served = pool.health_snapshot();
    assert_eq!(served.queued, 0);
    assert_eq!(served.queued_total, 1);
    assert_eq!(served.queue_cancellations, 0);
}

#[tokio::test]
async fn reusable_path_waiter_timeout_leaves_the_queue_empty() {
    let pool = RoundRobinPool::new_named_servers(
        "bounded".into(),
        [runtime_server("only", 3000, Some(1))],
        UpstreamAlgorithm::First,
        None,
        Some(Duration::from_millis(30)),
    )
    .expect("bounded pool");
    let first = connection_lifetime(&pool, "only");
    assert!(first.try_acquire().expect("initial acquisition"));
    let second = connection_lifetime(&pool, "only");
    let generation = second.capacity_generation();

    assert!(second.wait_for_capacity(generation).await.is_err());

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.queued_total, 1);
    assert_eq!(snapshot.queue_timeouts, 1);
}

struct ProbeStub {
    address: SocketAddr,
    requests: Arc<AtomicU64>,
    status: Arc<std::sync::atomic::AtomicU16>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ProbeStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn probe_stub(status: u16) -> ProbeStub {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("stub bind");
    let address = listener.local_addr().expect("stub address");
    let requests = Arc::new(AtomicU64::new(0));
    let status = Arc::new(std::sync::atomic::AtomicU16::new(status));
    let (seen, current) = (Arc::clone(&requests), Arc::clone(&status));
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("stub accept");
            let (seen, current) = (Arc::clone(&seen), Arc::clone(&current));
            tokio::spawn(async move {
                let mut buffer = [0_u8; 1024];
                if stream.read(&mut buffer).await.is_ok_and(|read| read > 0) {
                    seen.fetch_add(1, Ordering::SeqCst);
                    let status = current.load(Ordering::SeqCst);
                    let _ = stream
                        .write_all(
                            format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .as_bytes(),
                        )
                        .await;
                }
            });
        }
    });
    ProbeStub {
        address,
        requests,
        status,
        task,
    }
}

fn probed_pool(address: SocketAddr, successes: u16) -> RoundRobinPool {
    let readmission = oxiroute_config::Readmission {
        successes,
        interval_ms: 100,
        timeout_ms: 500,
        probe: Some(oxiroute_config::ReadmissionProbe {
            path: "/deep".into(),
            host: None,
            expected_status: None,
        }),
    };
    let blueprint =
        PoolConstructionBlueprint::compile(&UpstreamAlgorithm::First, 1, Some(&readmission), None)
            .expect("blueprint");
    let mut policy =
        PassiveFailurePolicy::new(1, Duration::from_millis(50), Duration::from_millis(200));
    policy.readmit_by = ReadmitBy::Probe;
    RoundRobinPool::acquire_compiled(
        "probed".into(),
        [RuntimeServer {
            name: "only".into(),
            endpoint: RuntimeEndpoint::from(address),
            max_connections: None,
            pinned_addresses: None,
            protected_addresses: Arc::from([]),
        }],
        Some(HealthStartup::Healthy),
        None,
        policy,
        PoolResiliencePolicy::default(),
        &blueprint,
    )
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn an_ejected_server_stays_out_until_its_probe_passes() {
    let stub = probe_stub(500).await;
    let pool = probed_pool(stub.address, 2);
    pool.record_passive_failure(0, HealthFailure::Timeout);

    wait_until("failed probes to extend the ejection", || {
        pool.health_snapshot().endpoints[0].passive_ejection_count >= 3
    })
    .await;
    let ejected = pool.health_snapshot();
    assert!(
        ejected.endpoints[0].passive_ejected,
        "the time ending is not enough"
    );
    assert!(pool.select().is_none());
    assert_eq!(ejected.endpoints[0].passive_recovery_count, 0);

    stub.status.store(200, Ordering::SeqCst);
    wait_until("the server to be re-admitted", || {
        !pool.health_snapshot().endpoints[0].passive_ejected
    })
    .await;
    assert!(pool.select().is_some());
    let recovered = pool.health_snapshot();
    assert_eq!(recovered.endpoints[0].passive_recovery_count, 1);
    assert!(
        stub.requests.load(Ordering::SeqCst) >= 4,
        "two passing probes after the failures"
    );
}

#[tokio::test]
async fn readmission_needs_the_configured_run_of_passing_probes() {
    let stub = probe_stub(200).await;
    let pool = probed_pool(stub.address, 3);
    pool.record_passive_failure(0, HealthFailure::Timeout);

    wait_until("re-admission", || {
        !pool.health_snapshot().endpoints[0].passive_ejected
    })
    .await;

    assert_eq!(stub.requests.load(Ordering::SeqCst), 3);
}

#[test]
fn active_health_checks_cannot_bypass_probe_readmission() {
    let policy = {
        let mut policy =
            PassiveFailurePolicy::new(1, Duration::from_millis(10), Duration::from_millis(10));
        policy.readmit_by = ReadmitBy::Probe;
        policy
    };
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "bypass".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("pool");
    pool.record_passive_failure(0, HealthFailure::Timeout);
    std::thread::sleep(Duration::from_millis(20));

    pool.record_health(&pool.handle(0), true, None, Some(now_unix_ms()), 1, 1);

    let snapshot = pool.health_snapshot();
    assert!(snapshot.endpoints[0].passive_ejected);
    assert!(pool.select().is_none());
}

#[test]
fn a_live_success_resets_consecutive_passive_failures() {
    let pool = RoundRobinPool::new_named_servers_with_policy(
        "passive-success".into(),
        [runtime_server("only", 3000, None)],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        PassiveFailurePolicy::new(3, Duration::from_secs(60), Duration::from_secs(60)),
    )
    .expect("passive success pool");
    let observation = pool.select().expect("lease").observation();

    for _ in 0..2 {
        pool.record_passive_failure(0, HealthFailure::Timeout);
    }
    observation.record_passive_success();
    for _ in 0..2 {
        pool.record_passive_failure(0, HealthFailure::Timeout);
    }

    let snapshot = pool.health_snapshot();
    assert!(!snapshot.endpoints[0].passive_ejected);
    assert_eq!(snapshot.endpoints[0].passive_consecutive_failures, 2);
    pool.record_passive_failure(0, HealthFailure::Timeout);
    assert!(pool.health_snapshot().endpoints[0].passive_ejected);
}

fn weighted_pair(algorithm: UpstreamAlgorithm) -> RoundRobinPool {
    RoundRobinPool::new_named_servers(
        "pair".into(),
        [
            runtime_server("a", 3000, None),
            runtime_server("b", 3001, None),
        ],
        algorithm,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pair pool")
}

fn picks(pool: &RoundRobinPool, count: usize) -> Vec<String> {
    (0..count)
        .map(|_| pool.select().expect("lease").server_name().to_owned())
        .collect()
}

#[test]
fn a_runtime_weight_changes_the_weighted_split_and_reset_restores_it() {
    let pool = weighted_pair(UpstreamAlgorithm::WeightedRoundRobin {
        weights: vec![1, 1],
    });
    let share = |names: &[String]| names.iter().filter(|name| *name == "b").count();

    assert_eq!(share(&picks(&pool, 8)), 4);
    pool.set_server_weight("b", Some(3)).expect("raise weight");
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.endpoints[1].weight, 3);
    assert_eq!(snapshot.endpoints[1].configured_weight, 1);
    assert_eq!(share(&picks(&pool, 8)), 6);
    pool.set_server_weight("b", None).expect("reset weight");
    assert_eq!(pool.health_snapshot().endpoints[1].weight, 1);
    assert_eq!(share(&picks(&pool, 8)), 4);
}

#[test]
fn runtime_weights_are_bounded_and_carried_to_the_next_generation() {
    let pool = weighted_pair(UpstreamAlgorithm::LeastConnections);
    assert_eq!(
        pool.set_server_weight("a", Some(0)),
        Err(PoolAdminError::InvalidWeight)
    );
    assert_eq!(
        pool.set_server_weight("a", Some(101)),
        Err(PoolAdminError::InvalidWeight)
    );
    assert!(pool.set_server_weight("missing", Some(2)).is_err());
    pool.set_server_weight("a", Some(7)).expect("weight");

    let carried = pool.administrative_states();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].weight, Some(7));
    let next = weighted_pair(UpstreamAlgorithm::LeastConnections);
    next.restore_administrative_states(&carried);
    assert_eq!(next.health_snapshot().endpoints[0].weight, 7);
}

#[test]
fn least_connections_compares_load_per_unit_of_weight() {
    let pool = weighted_pair(UpstreamAlgorithm::LeastConnections);
    pool.set_server_weight("a", Some(3)).expect("weight");
    let leases = (0..8)
        .map(|_| pool.select().expect("lease"))
        .collect::<Vec<_>>();
    let on_a = leases
        .iter()
        .filter(|lease| lease.server_name() == "a")
        .count();

    assert_eq!(on_a, 6, "a weight-3 server carries three times the work");
}

#[test]
fn power_of_two_choices_never_picks_the_busier_of_two_servers() {
    let pool = RoundRobinPool::new_named_servers(
        "p2c".into(),
        [
            runtime_server("s0", 3000, None),
            runtime_server("s1", 3001, None),
            runtime_server("s2", 3002, None),
        ],
        UpstreamAlgorithm::PowerOfTwoChoices,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("p2c pool");
    let _busy = (0..5)
        .map(|_| pool.select_named_server("s0").lease.expect("busy lease"))
        .collect::<Vec<_>>();

    let chosen = (0..200)
        .map(|_| pool.select().expect("lease").server_name().to_owned())
        .collect::<Vec<_>>();

    assert!(chosen.iter().all(|name| name != "s0"));
    assert!(chosen.iter().any(|name| name == "s1"));
    assert!(chosen.iter().any(|name| name == "s2"));
    assert_eq!(pool.health_snapshot().algorithm, "power_of_two_choices");
}

fn breaker_pool() -> RoundRobinPool {
    let breaker = CircuitBreakerPolicy::from(&oxiroute_config::CircuitBreaker {
        window_ms: 10_000,
        min_requests: 10,
        failure_ratio_percent: 50,
        open_ms: 100,
        half_open_max: 1,
        half_open_successes: 3,
    });
    let mut policy =
        PassiveFailurePolicy::new(100, Duration::from_secs(60), Duration::from_secs(60));
    policy.circuit_breaker = Some(breaker);
    RoundRobinPool::new_named_servers_with_policy(
        "breaker".into(),
        [
            runtime_server("only", 3000, None),
            runtime_server("other", 3001, None),
        ],
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("breaker pool")
}

fn record(pool: &RoundRobinPool, ok: usize, failed: usize) {
    let observation = pool.select().expect("lease").observation();
    for _ in 0..ok {
        observation.record_passive_success();
    }
    for _ in 0..failed {
        pool.record_passive_failure(0, HealthFailure::Timeout);
    }
}

fn first_server(pool: &RoundRobinPool) -> Option<String> {
    pool.select().map(|lease| lease.server_name().to_owned())
}

#[test]
fn a_failure_ratio_below_the_threshold_never_opens_the_breaker() {
    let pool = breaker_pool();

    record(&pool, 7, 3);

    assert_eq!(first_server(&pool).as_deref(), Some("only"));
}

#[tokio::test]
async fn a_failure_ratio_above_the_threshold_opens_then_closes_after_trial_successes() {
    let pool = breaker_pool();
    record(&pool, 4, 6);
    assert_eq!(
        first_server(&pool).as_deref(),
        Some("other"),
        "the open server is skipped"
    );

    tokio::time::sleep(Duration::from_millis(150)).await;
    let trial = pool.select().expect("trial lease");
    assert_eq!(trial.server_name(), "only", "half-open admits a trial");
    assert_eq!(
        pool.select().expect("second lease").server_name(),
        "other",
        "only one trial runs at a time"
    );
    drop(trial);
    for _ in 0..3 {
        let lease = pool.select().expect("trial lease");
        assert_eq!(lease.server_name(), "only");
        lease.observation().record_passive_success();
    }

    let leases = [pool.select(), pool.select()];
    assert!(leases.iter().all(|lease| {
        lease
            .as_ref()
            .is_some_and(|lease| lease.server_name() == "only")
    }));
}

#[tokio::test]
async fn a_failed_trial_reopens_the_breaker() {
    let pool = breaker_pool();
    record(&pool, 0, 10);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(first_server(&pool).as_deref(), Some("only"));

    pool.record_passive_failure(0, HealthFailure::Timeout);

    assert_eq!(first_server(&pool).as_deref(), Some("other"));
}

fn outlier_pool(servers: u16, max_ejected_percent: u8) -> RoundRobinPool {
    let mut policy =
        PassiveFailurePolicy::new(100, Duration::from_secs(60), Duration::from_secs(60));
    policy.outlier = Some(OutlierPolicy::from(&oxiroute_config::OutlierDetection {
        factor_percent: 300,
        min_samples: 5,
        interval_ms: 1_000,
        max_ejected_percent,
    }));
    RoundRobinPool::new_named_servers_with_policy(
        "outliers".into(),
        (0..servers).map(|index| runtime_server(&format!("s{index}"), 3000 + index, None)),
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
        policy,
    )
    .expect("outlier pool")
}

/// One interval of traffic: `samples` responses per server and route, then the judging response.
fn feed(pool: &RoundRobinPool, route: &str, millis: &[u64], interval: u64) {
    let at = now_unix_ms() - 10_000 + interval * 1_000;
    for _ in 0..6 {
        for (index, ms) in millis.iter().enumerate() {
            let server = &pool.health.members.load().endpoints[index];
            pool.health
                .record_latency_at(server, route, Duration::from_millis(*ms), at);
        }
    }
}

fn ejected(pool: &RoundRobinPool) -> Vec<(usize, Option<&'static str>)> {
    pool.health_snapshot()
        .endpoints
        .iter()
        .enumerate()
        .filter(|(_, endpoint)| endpoint.passive_ejected)
        .map(|(index, endpoint)| {
            (
                index,
                endpoint.passive_ejection_reason.map(HealthFailure::as_str),
            )
        })
        .collect()
}

#[test]
fn a_server_far_slower_than_its_peers_on_a_route_is_ejected() {
    let pool = outlier_pool(3, 50);

    feed(&pool, "run", &[10, 12, 100], 0);
    assert!(ejected(&pool).is_empty(), "no interval has ended yet");
    feed(&pool, "run", &[10, 12, 100], 1);

    assert_eq!(ejected(&pool), [(2, Some("slow"))]);
}

#[test]
fn a_moderately_slower_server_stays_in_rotation() {
    let pool = outlier_pool(3, 50);

    feed(&pool, "run", &[10, 12, 25], 0);
    feed(&pool, "run", &[10, 12, 25], 1);

    assert!(ejected(&pool).is_empty());
}

#[test]
fn outlier_ejection_never_exceeds_the_ejected_share() {
    let pool = outlier_pool(4, 30);

    feed(&pool, "run", &[10, 10, 100, 100], 0);
    feed(&pool, "run", &[10, 10, 100, 100], 1);

    assert_eq!(ejected(&pool).len(), 1, "30% of four servers allows one");
}

#[test]
fn routes_are_judged_separately() {
    let pool = outlier_pool(3, 50);

    for interval in 0..2 {
        feed(&pool, "draft", &[10, 10, 10], interval);
        feed(&pool, "render", &[3_000, 3_000, 3_000], interval);
    }

    assert!(ejected(&pool).is_empty(), "long jobs are not slow servers");
}

fn classed_pool(discipline: oxiroute_config::QueueDiscipline) -> Arc<RoundRobinPool> {
    let class = |name: &str, weight| oxiroute_config::PoolQueueClass {
        name: name.into(),
        weight,
    };
    let policy = PoolQueuePolicy {
        classes: vec![class("interactive", 8), class("batch", 1)],
        discipline,
        class_header: Some("x-priority".into()),
        default_class: Some("batch".into()),
        tenant_header: None,
        max_tenants: 8,
        aging_ms: None,
    };
    let blueprint =
        PoolConstructionBlueprint::compile(&UpstreamAlgorithm::First, 1, None, Some(&policy))
            .expect("blueprint");
    Arc::new(RoundRobinPool::acquire_compiled(
        "classed".into(),
        [runtime_server("only", 3000, Some(1))],
        None,
        Some(Duration::from_secs(5)),
        PassiveFailurePolicy::default(),
        PoolResiliencePolicy::default(),
        &blueprint,
    ))
}

fn queue_as(
    pool: &Arc<RoundRobinPool>,
    class: &str,
) -> tokio::task::JoinHandle<Option<EndpointLease>> {
    let mut headers = HeaderMap::new();
    headers.insert("x-priority", class.parse().expect("class"));
    let tag = pool.queue_tag(None, &headers);
    let pool = Arc::clone(pool);
    tokio::spawn(async move { pool.select_wait_excluding(&[], tag).await })
}

#[tokio::test]
async fn a_higher_class_overtakes_earlier_lower_class_waiters_when_capacity_frees() {
    for discipline in [
        oxiroute_config::QueueDiscipline::Strict,
        oxiroute_config::QueueDiscipline::Weighted,
    ] {
        let pool = classed_pool(discipline);
        let held = pool.select().expect("capacity");
        let batch = queue_as(&pool, "batch");
        super::queue::wait_for_queued(&pool, 1).await;
        let interactive = queue_as(&pool, "interactive");
        super::queue::wait_for_queued(&pool, 2).await;
        assert_eq!(
            pool.health_snapshot().queued_by_class,
            [("interactive".to_owned(), 1), ("batch".to_owned(), 1)]
        );

        drop(held);

        let served = tokio::time::timeout(Duration::from_secs(1), interactive)
            .await
            .expect("interactive served first")
            .expect("task")
            .expect("lease");
        assert!(
            !batch.is_finished(),
            "the earlier batch waiter keeps waiting"
        );
        drop(served);
        let batch = batch.await.expect("task").expect("batch lease");
        drop(batch);
        assert_eq!(pool.health_snapshot().queued, 0);
    }
}
