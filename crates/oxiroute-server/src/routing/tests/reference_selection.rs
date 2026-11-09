//! The published selectable view must choose exactly what the original scan of every server chose.
//!
//! The reference below is the original selection (rotation-ordered candidates, a stable ranking,
//! a commit of the weighted credits and the rotation cursor), run against the live state of the
//! pool. Operations that change what is selectable, the capacity or the weights are interleaved
//! with selections so that every state change must retire the published view.

#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy)]
struct Candidate {
    index: usize,
    load: u64,
    weight: u16,
}

struct Reference {
    next: usize,
    credit: Vec<i64>,
}

impl Reference {
    fn new(servers: usize) -> Self {
        Self {
            next: 0,
            credit: vec![0; servers],
        }
    }

    fn candidates(
        &self,
        pool: &EndpointPool,
        excluded: &[String],
        now_unix_ms: u64,
    ) -> Vec<Candidate> {
        let members = pool.health.members.load_full();
        let start = match pool.algorithm {
            UpstreamAlgorithm::WeightedRoundRobin { .. } => 0,
            _ => self.next,
        };
        (0..members.endpoints.len())
            .filter_map(|offset| {
                let index = (start + offset) % members.endpoints.len();
                let server = &members.endpoints[index];
                (server.selectable_at(now_unix_ms) && !excluded.contains(&server.name)).then(|| {
                    Candidate {
                        index,
                        load: server.load(),
                        weight: server.weight(),
                    }
                })
            })
            .collect()
    }

    fn rank(&self, pool: &EndpointPool, candidates: &mut [Candidate]) {
        match &pool.algorithm {
            UpstreamAlgorithm::RoundRobin => {}
            UpstreamAlgorithm::First => candidates.sort_by_key(|candidate| candidate.index),
            UpstreamAlgorithm::LeastConnections => {
                candidates.sort_by_key(|candidate| candidate.load);
            }
            UpstreamAlgorithm::WeightedRoundRobin { .. } => candidates.sort_by_key(|candidate| {
                (
                    std::cmp::Reverse(self.credit[candidate.index] + i64::from(candidate.weight)),
                    candidate.index,
                )
            }),
            UpstreamAlgorithm::PowerOfTwoChoices => unreachable!("random"),
        }
    }

    fn commit(&mut self, pool: &EndpointPool, candidates: &[Candidate], chosen: usize) {
        if matches!(
            &pool.algorithm,
            UpstreamAlgorithm::WeightedRoundRobin { .. }
        ) {
            let total = candidates.iter().map(|c| i64::from(c.weight)).sum::<i64>();
            for candidate in candidates {
                self.credit[candidate.index] += i64::from(candidate.weight);
            }
            self.credit[chosen] -= total;
        }
        if !matches!(&pool.algorithm, UpstreamAlgorithm::First) {
            self.next = (chosen + 1) % pool.health.members.load_full().endpoints.len();
        }
    }

    /// The server `select_server_excluding` takes a permit of.
    fn acquire(&mut self, pool: &EndpointPool, excluded: &[String]) -> Option<usize> {
        let members = pool.health.members.load_full();
        let candidates = self.candidates(pool, excluded, now_unix_ms());
        let mut ordered = candidates
            .into_iter()
            .filter(|candidate| members.endpoints[candidate.index].has_capacity())
            .collect::<Vec<_>>();
        let pooled = ordered.clone();
        self.rank(pool, &mut ordered);
        let chosen = ordered.first()?.index;
        self.commit(pool, &pooled, chosen);
        Some(chosen)
    }

    /// The server a request that may reuse an idle pooled connection is bound to.
    fn reuse(&mut self, pool: &EndpointPool, excluded: &[String]) -> Option<usize> {
        let members = pool.health.members.load_full();
        let mut pooled = self.candidates(pool, excluded, now_unix_ms());
        pooled.retain(|candidate| {
            let server = &members.endpoints[candidate.index];
            server.has_capacity() || server.has_idle_connection()
        });
        let all = pooled.clone();
        self.rank(pool, &mut pooled);
        let chosen = pooled.first()?.index;
        self.commit(pool, &all, chosen);
        Some(chosen)
    }

    /// The server `connection_target` hands a pending lease of.
    fn target(&mut self, pool: &EndpointPool, excluded: &[String]) -> Option<usize> {
        let members = pool.health.members.load_full();
        let candidates = self.candidates(pool, excluded, now_unix_ms());
        let available = candidates
            .iter()
            .copied()
            .filter(|candidate| members.endpoints[candidate.index].has_capacity())
            .collect::<Vec<_>>();
        let mut pooled = if available.is_empty() {
            candidates
        } else {
            available
        };
        let all = pooled.clone();
        self.rank(pool, &mut pooled);
        let chosen = pooled.first()?.index;
        self.commit(pool, &all, chosen);
        Some(chosen)
    }
}

struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % bound as u64).expect("bounded")
    }
}

const SERVERS: usize = 9;

fn small(value: usize) -> u16 {
    u16::try_from(value).expect("small")
}

fn pool_of(algorithm: UpstreamAlgorithm, passive: PassiveFailurePolicy) -> EndpointPool {
    queued_pool_of(algorithm, passive, None)
}

fn queued_pool_of(
    algorithm: UpstreamAlgorithm,
    passive: PassiveFailurePolicy,
    queue_timeout: Option<Duration>,
) -> EndpointPool {
    EndpointPool::new_named_servers_resilient(
        "reference".into(),
        (0..SERVERS).map(|index| runtime_server(&format!("s{index}"), 4000 + small(index), None)),
        algorithm,
        Some(HealthStartup::Healthy),
        queue_timeout,
        passive,
        PoolResiliencePolicy::default(),
    )
    .expect("pool")
}

fn name(index: usize) -> String {
    format!("s{index}")
}

/// Pooled keep-alive connections: they hold a permit, and idle ones can be reused without one.
fn pool_connection(
    pool: &EndpointPool,
    random: &mut Random,
    connections: &mut Vec<Arc<EndpointLeaseInner>>,
) {
    match random.below(4) {
        0 | 1 => {
            let lease = pool.select_server_connection_target(&name(random.below(SERVERS)));
            if let Some(connection) = lease.map(|lease| Arc::clone(&lease.inner))
                && connection.try_acquire().expect("acquire")
            {
                connection.notify_reusable();
                connections.push(connection);
            }
        }
        2 if !connections.is_empty() => {
            connections[random.below(connections.len())].notify_in_use();
        }
        _ if !connections.is_empty() => {
            connections.swap_remove(random.below(connections.len()));
        }
        _ => {}
    }
}

/// Changes something selection depends on, through the pool's own entry points.
fn disturb(pool: &EndpointPool, random: &mut Random, held: &mut Vec<EndpointLease>) {
    let server = name(random.below(SERVERS));
    match random.below(10) {
        0 => pool
            .set_server_administrative_state(&server, AdministrativeState::Drain)
            .expect("drain"),
        1 | 2 => pool
            .set_server_administrative_state(&server, AdministrativeState::Ready)
            .expect("ready"),
        3 => pool
            .set_server_health_override(&server, HealthOverride::Down)
            .expect("down"),
        4 => pool
            .set_server_health_override(&server, HealthOverride::Auto)
            .expect("auto"),
        5 => pool
            .set_server_weight(&server, Some(1 + small(random.below(9))))
            .expect("weight"),
        6 => pool
            .set_server_max_connections(&server, [Some(1), Some(3), None][random.below(3)])
            .expect("limit"),
        7 => {
            let handle = pool.server_handle(&server).expect("handle");
            let healthy = random.below(2) == 0;
            pool.record_health(
                &handle,
                healthy,
                Some(HealthFailure::Timeout),
                Some(1),
                1,
                1,
            );
        }
        8 => {
            if !held.is_empty() {
                held.swap_remove(random.below(held.len()));
            }
        }
        _ => pool.record_passive_failure(random.below(SERVERS), HealthFailure::Timeout),
    }
}

fn run(algorithm: UpstreamAlgorithm, passive: PassiveFailurePolicy, seed: u64) {
    run_reusing(algorithm, passive, seed, None);
}

/// With a queue timeout, requests may also be bound to idle pooled connections.
fn run_reusing(
    algorithm: UpstreamAlgorithm,
    passive: PassiveFailurePolicy,
    seed: u64,
    queue_timeout: Option<Duration>,
) {
    let pool = queued_pool_of(algorithm, passive, queue_timeout);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut reference = Reference::new(SERVERS);
    let mut random = Random(seed);
    let mut held = Vec::new();
    let mut connections = Vec::new();
    for step in 0..4000 {
        let excluded = (0..random.below(3))
            .map(|_| name(random.below(SERVERS)))
            .collect::<Vec<_>>();
        if queue_timeout.is_some() && random.below(3) == 0 {
            let expected = reference.reuse(&pool, &excluded).map(name);
            let lease = runtime
                .block_on(pool.select_connection_target_waiting(&excluded, QueueTag::default()));
            assert_eq!(
                lease.as_ref().map(|lease| lease.server_name().to_owned()),
                expected,
                "reuse at step {step}"
            );
            held.extend(lease.filter(|_| random.below(3) != 0));
        } else if random.below(2) == 0 {
            let expected = reference.acquire(&pool, &excluded).map(name);
            let lease = pool.select_server_excluding(&excluded).lease;
            assert_eq!(
                lease.as_ref().map(|lease| lease.server_name().to_owned()),
                expected,
                "acquire at step {step}"
            );
            held.extend(lease.filter(|_| random.below(3) != 0));
        } else {
            let expected = reference.target(&pool, &excluded).map(name);
            let lease = pool.select_connection_target_excluding(&excluded);
            assert_eq!(
                lease.as_ref().map(|lease| lease.server_name().to_owned()),
                expected,
                "target at step {step}"
            );
        }
        if random.below(3) == 0 {
            disturb(&pool, &mut random, &mut held);
        }
        if queue_timeout.is_some() && random.below(2) == 0 {
            pool_connection(&pool, &mut random, &mut connections);
        }
    }
}

fn weighted() -> UpstreamAlgorithm {
    UpstreamAlgorithm::WeightedRoundRobin {
        weights: (0..SERVERS).map(|index| 1 + small(index % 4)).collect(),
    }
}

fn ejecting() -> PassiveFailurePolicy {
    PassiveFailurePolicy::new(1, Duration::from_hours(1), Duration::from_hours(1))
}

#[test]
fn round_robin_chooses_like_the_scan_of_every_server() {
    for seed in 1..=4 {
        run(UpstreamAlgorithm::RoundRobin, ejecting(), seed);
    }
}

#[test]
fn first_chooses_like_the_scan_of_every_server() {
    for seed in 1..=4 {
        run(UpstreamAlgorithm::First, ejecting(), seed);
    }
}

#[test]
fn least_connections_chooses_like_the_scan_of_every_server() {
    for seed in 1..=4 {
        run(UpstreamAlgorithm::LeastConnections, ejecting(), seed);
    }
}

#[test]
fn every_algorithm_chooses_like_the_scan_when_idle_connections_can_be_reused() {
    for seed in 1..=4 {
        for algorithm in [
            UpstreamAlgorithm::RoundRobin,
            UpstreamAlgorithm::First,
            UpstreamAlgorithm::LeastConnections,
            weighted(),
        ] {
            run_reusing(algorithm, ejecting(), seed, Some(Duration::from_millis(1)));
        }
    }
}

#[test]
fn weighted_round_robin_chooses_like_the_scan_of_every_server() {
    for seed in 1..=4 {
        run(weighted(), ejecting(), seed);
    }
}

#[test]
fn power_of_two_choices_only_chooses_admissible_servers_and_avoids_the_busiest() {
    let pool = pool_of(
        UpstreamAlgorithm::PowerOfTwoChoices,
        PassiveFailurePolicy::default(),
    );
    let _held = [4_usize, 4, 4, 9, 4, 4, 4, 4, 4]
        .into_iter()
        .enumerate()
        .flat_map(|(index, load)| (0..load).map(move |_| index))
        .filter_map(|index| pool.select_named_server(&name(index)).lease)
        .collect::<Vec<_>>();
    pool.set_server_administrative_state("s1", AdministrativeState::Drain)
        .expect("drain");
    let excluded = [name(2)];
    let mut picks = [0_u32; SERVERS];
    for _ in 0..2000 {
        let lease = pool
            .select_connection_target_excluding(&excluded)
            .expect("lease");
        picks[lease.server_name()[1..].parse::<usize>().expect("index")] += 1;
    }
    assert_eq!(picks[1], 0, "drained server chosen");
    assert_eq!(picks[2], 0, "excluded server chosen");
    assert_eq!(picks[3], 0, "the busiest server wins no comparison");
    assert!(
        [0, 4, 5, 6, 7, 8].iter().all(|&index| picks[index] > 0),
        "{picks:?}"
    );
}

#[test]
fn a_timed_ejection_ends_without_any_event() {
    let policy = PassiveFailurePolicy::new(1, Duration::from_millis(60), Duration::from_millis(60));
    let pool = pool_of(UpstreamAlgorithm::First, policy);
    assert_eq!(pool.select().expect("lease").server_name(), "s0");
    pool.record_passive_failure(0, HealthFailure::Timeout);
    assert_eq!(pool.select().expect("lease").server_name(), "s1");
    thread::sleep(Duration::from_millis(80));
    assert_eq!(pool.select().expect("lease").server_name(), "s0");
}

#[test]
fn an_open_breaker_closes_its_view_without_any_event() {
    let breaker = CircuitBreakerPolicy::from(&oxiroute_config::CircuitBreaker {
        window_ms: 10_000,
        min_requests: 10,
        failure_ratio_percent: 50,
        open_ms: 60,
        half_open_max: 1,
        half_open_successes: 3,
    });
    let mut policy = PassiveFailurePolicy::new(100, Duration::from_mins(1), Duration::from_mins(1));
    policy.circuit_breaker = Some(breaker);
    let pool = pool_of(UpstreamAlgorithm::First, policy);
    assert_eq!(pool.select().expect("lease").server_name(), "s0");
    for _ in 0..20 {
        pool.record_passive_failure(0, HealthFailure::Timeout);
    }
    assert_eq!(pool.select().expect("lease").server_name(), "s1");
    thread::sleep(Duration::from_millis(80));
    assert_eq!(pool.select().expect("lease").server_name(), "s0");
}
