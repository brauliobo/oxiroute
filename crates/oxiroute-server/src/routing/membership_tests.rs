use std::{sync::Arc, thread, time::Duration};

use oxiroute_config::{HealthStartup, UpstreamAlgorithm};

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

fn dynamic(weight: u16, priority: u16) -> DynamicServer {
    DynamicServer { weight, priority }
}

fn pool(algorithm: UpstreamAlgorithm, statics: &[(&str, u16)]) -> EndpointPool {
    EndpointPool::new_named_servers(
        "render".into(),
        statics
            .iter()
            .map(|(name, port)| runtime_server(name, *port, None)),
        algorithm,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool")
}

fn names(pool: &EndpointPool) -> Vec<String> {
    pool.health_snapshot()
        .endpoints
        .into_iter()
        .map(|endpoint| endpoint.name)
        .collect()
}

#[test]
fn a_pool_without_servers_is_unavailable_until_one_joins() {
    let pool = pool(UpstreamAlgorithm::RoundRobin, &[]);
    assert!(pool.select().is_none());
    assert_eq!(pool.health_snapshot().total_endpoints, 0);

    pool.add_server(runtime_server("late", 4000, None), dynamic(1, 100))
        .expect("add");

    assert_eq!(pool.select().expect("joined").server_name(), "late");
}

#[test]
fn dynamic_servers_follow_static_ones_ordered_by_priority_then_name() {
    let pool = pool(UpstreamAlgorithm::First, &[("local", 3000)]);
    pool.add_server(runtime_server("zeta", 4001, None), dynamic(1, 50))
        .expect("zeta");
    pool.add_server(runtime_server("beta", 4002, None), dynamic(1, 100))
        .expect("beta");
    pool.add_server(runtime_server("alpha", 4003, None), dynamic(1, 100))
        .expect("alpha");

    assert_eq!(names(&pool), ["local", "zeta", "alpha", "beta"]);
    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.endpoints[0].origin, ServerOrigin::Static);
    assert_eq!(snapshot.endpoints[1].origin, ServerOrigin::Dynamic);
    let held = pool.select().expect("local first");
    assert_eq!(held.server_name(), "local");
}

#[test]
fn a_duplicate_name_is_refused_and_leaves_membership_unchanged() {
    let pool = pool(UpstreamAlgorithm::RoundRobin, &[("one", 3000)]);

    let error = pool
        .add_server(runtime_server("one", 4000, None), dynamic(1, 100))
        .expect_err("duplicate");

    assert!(matches!(error, PoolMembershipError::DuplicateName(name) if name == "one"));
    assert_eq!(names(&pool), ["one"]);
}

#[test]
fn a_removed_server_stops_taking_work_while_its_leases_finish() {
    let pool = pool(UpstreamAlgorithm::First, &[("one", 3000)]);
    pool.add_server(runtime_server("two", 4000, Some(2)), dynamic(1, 100))
        .expect("two");
    let first = pool.select().expect("one");
    assert_eq!(first.server_name(), "one");
    let second = pool.select().expect("one again");
    drop(second);
    pool.remove_server("one").expect("remove");

    assert_eq!(names(&pool), ["two"]);
    assert_eq!(pool.select().expect("two").server_name(), "two");
    drop(first);
    assert!(matches!(
        pool.remove_server("one"),
        Err(PoolMembershipError::UnknownServer(_))
    ));
}

#[test]
fn rejoining_with_the_same_endpoint_keeps_work_and_health_through_update() {
    let pool = pool(UpstreamAlgorithm::RoundRobin, &[]);
    pool.add_server(runtime_server("gpu", 4000, Some(1)), dynamic(1, 100))
        .expect("gpu");
    let held = pool.select().expect("lease");
    pool.record_health(
        &pool.server_handle("gpu").expect("handle"),
        true,
        None,
        Some(7),
        1,
        1,
    );

    pool.update_server("gpu", dynamic(5, 100), Some(3))
        .expect("update");

    let snapshot = pool.health_snapshot().endpoints.remove(0);
    assert_eq!(snapshot.active_connections, 1);
    assert_eq!(snapshot.weight, 5);
    assert_eq!(snapshot.configured_max_connections, Some(3));
    assert_eq!(snapshot.successful_checks, 1);
    drop(held);
}

#[test]
fn weighted_rotation_includes_dynamic_servers_and_follows_weight_updates() {
    let pool = pool(
        UpstreamAlgorithm::WeightedRoundRobin { weights: vec![1] },
        &[("static", 3000)],
    );
    pool.add_server(runtime_server("heavy", 4000, None), dynamic(3, 100))
        .expect("heavy");
    let count = |pool: &EndpointPool| {
        let mut heavy = 0;
        for _ in 0..8 {
            heavy += usize::from(pool.select().expect("lease").server_name() == "heavy");
        }
        heavy
    };
    assert_eq!(count(&pool), 6);

    pool.update_server("heavy", dynamic(1, 100), None)
        .expect("lighter");
    assert_eq!(count(&pool), 4);
    pool.remove_server("heavy").expect("remove");
    assert_eq!(count(&pool), 0);
}

#[test]
fn the_round_robin_cursor_stays_valid_when_the_tail_member_leaves() {
    let pool = pool(UpstreamAlgorithm::RoundRobin, &[("a", 3000), ("b", 3001)]);
    pool.add_server(runtime_server("c", 4000, None), dynamic(1, 100))
        .expect("c");
    for _ in 0..2 {
        drop(pool.select().expect("advance"));
    }
    pool.remove_server("c").expect("remove");

    let picked = (0..4)
        .map(|_| pool.select().expect("lease").server_name().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(picked, ["a", "b", "a", "b"]);
}

#[tokio::test]
async fn a_server_joining_wakes_requests_queued_on_a_saturated_pool() {
    let pool = Arc::new(
        EndpointPool::new_named_servers(
            "render".into(),
            [runtime_server("only", 3000, Some(1))],
            UpstreamAlgorithm::First,
            None,
            Some(Duration::from_secs(5)),
        )
        .expect("pool"),
    );
    let held = pool.select().expect("capacity");
    let waiting = Arc::clone(&pool);
    let waiter = tokio::spawn(async move { waiting.select_wait().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while pool.health_snapshot().queued != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued");

    pool.add_server(runtime_server("fresh", 4000, None), dynamic(1, 100))
        .expect("join");

    let lease = waiter
        .await
        .expect("task")
        .expect("served by the new server");
    assert_eq!(lease.server_name(), "fresh");
    drop(held);
}

#[test]
fn selections_never_observe_a_torn_membership_during_churn() {
    let pool = Arc::new(pool(UpstreamAlgorithm::RoundRobin, &[("base", 3000)]));
    let writer = {
        let pool = Arc::clone(&pool);
        thread::spawn(move || {
            for round in 0..300_u16 {
                let name = format!("dyn-{}", round % 7);
                let _ = pool.add_server(
                    runtime_server(&name, 4000 + round % 7, None),
                    dynamic(1, 100),
                );
                let _ = pool.remove_server(&format!("dyn-{}", (round + 3) % 7));
            }
        })
    };
    let readers = (0..3)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for _ in 0..3_000 {
                    let lease = pool.select().expect("base is always a member");
                    let snapshot = pool.health_snapshot();
                    assert_eq!(snapshot.total_endpoints, snapshot.endpoints.len());
                    drop(lease);
                }
            })
        })
        .collect::<Vec<_>>();
    writer.join().expect("writer");
    for reader in readers {
        reader.join().expect("reader");
    }
    assert_eq!(
        pool.health_snapshot()
            .endpoints
            .iter()
            .map(|endpoint| endpoint.active_connections)
            .sum::<u64>(),
        0
    );
}
