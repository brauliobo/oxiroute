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

/// What `install` used to compute by looking every server up in the previous membership.
fn carried_over(
    previous: &[Arc<PoolEndpoint>],
    credit: &[i64],
    next: usize,
    endpoints: &[Arc<PoolEndpoint>],
) -> (Vec<i64>, usize) {
    let position = |members: &[Arc<PoolEndpoint>], server: &Arc<PoolEndpoint>| {
        members
            .iter()
            .position(|member| Arc::ptr_eq(member, server))
    };
    let carried = endpoints
        .iter()
        .map(|server| position(previous, server).map_or(0, |index| credit[index]))
        .collect();
    let cursor = previous
        .get(next)
        .and_then(|server| position(endpoints, server))
        .unwrap_or(next);
    (carried, cursor.checked_rem(endpoints.len()).unwrap_or(0))
}

#[test]
fn membership_changes_carry_credits_and_the_cursor_like_a_full_lookup_would() {
    let pool = pool(
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![1, 1],
        },
        &[("s0", 3000), ("s1", 3001)],
    );
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut random = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % u64::try_from(bound).expect("bound")).expect("index")
    };
    for step in 0..600 {
        {
            let mut selection = pool.selection.lock().expect("selection");
            for credit in &mut selection.credit {
                *credit = i64::try_from(random(50)).expect("credit") - 25;
            }
            selection.next = random(selection.credit.len().max(1));
        }
        let previous = pool.health.members.load_full().endpoints.to_vec();
        let (credit, next) = {
            let selection = pool.selection.lock().expect("selection");
            (selection.credit.clone(), selection.next)
        };
        let name = format!("d{}", random(12));
        let known = previous.iter().any(|member| member.name == name);
        match random(3) {
            0 if !known => {
                pool.add_server(
                    runtime_server(&name, 4000, None),
                    dynamic(1, u16::try_from(random(4)).expect("priority")),
                )
                .expect("add");
            }
            1 if known => {
                pool.remove_server(&name).expect("remove");
            }
            2 if known => {
                let priority = u16::try_from(random(4)).expect("priority");
                pool.update_server(&name, dynamic(1, priority), None)
                    .expect("update");
            }
            _ => continue,
        }
        let after = pool.health.members.load_full().endpoints.to_vec();
        let selection = pool.selection.lock().expect("selection");
        let expected = carried_over(&previous, &credit, next, &after);
        assert_eq!(
            (selection.credit.clone(), selection.next),
            expected,
            "step {step}"
        );
        let tail = after
            .iter()
            .filter(|member| member.origin == ServerOrigin::Dynamic);
        let keys = tail
            .map(|member| (member.priority(), member.name.clone()))
            .collect::<Vec<_>>();
        assert!(keys.is_sorted(), "dynamic servers stay ordered: {keys:?}");
    }
}

#[test]
fn an_update_that_changes_nothing_publishes_and_wakes_nobody() {
    let pool = pool(UpstreamAlgorithm::RoundRobin, &[("one", 3000)]);
    pool.add_server(runtime_server("gpu", 4000, Some(2)), dynamic(3, 100))
        .expect("gpu");
    let (members, wakes) = (
        pool.health.members.load_full(),
        pool.queue.notifications.load(Ordering::Relaxed),
    );

    pool.update_server("gpu", dynamic(3, 100), Some(2))
        .expect("same");

    assert!(Arc::ptr_eq(&members, &pool.health.members.load_full()));
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), wakes);
    pool.update_server("gpu", dynamic(4, 100), Some(2))
        .expect("weight");
    assert!(
        Arc::ptr_eq(&members, &pool.health.members.load_full()),
        "a new weight changes no order"
    );
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), wakes + 1);
    pool.update_server("gpu", dynamic(4, 50), Some(2))
        .expect("priority");
    assert!(!Arc::ptr_eq(&members, &pool.health.members.load_full()));
}

#[test]
fn setting_the_state_a_server_already_has_wakes_nobody() {
    let pool = pool(UpstreamAlgorithm::RoundRobin, &[("one", 3000)]);
    let wakes = pool.queue.notifications.load(Ordering::Relaxed);

    pool.set_server_administrative_state("one", AdministrativeState::Ready)
        .expect("same state");
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), wakes);
    pool.set_server_administrative_state("one", AdministrativeState::Drain)
        .expect("drain");
    assert_eq!(pool.queue.notifications.load(Ordering::Relaxed), wakes + 1);
}

#[test]
fn several_servers_join_in_one_membership_change_and_a_duplicate_is_refused_alone() {
    let pool = pool(UpstreamAlgorithm::First, &[("local", 3000)]);
    let before = pool.health.members.load_full();
    let join = |name: &str, port, priority, state| NewMember {
        server: runtime_server(name, port, None),
        spec: dynamic(1, priority),
        state,
    };

    let results = pool.add_servers(vec![
        join("b", 4001, 100, AdministrativeState::Ready),
        join("a", 4002, 100, AdministrativeState::Drain),
        join("b", 4003, 100, AdministrativeState::Ready),
        join("z", 4004, 10, AdministrativeState::Ready),
    ]);

    assert!(matches!(&results[2], Err(PoolMembershipError::DuplicateName(name)) if name == "b"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 3);
    assert_eq!(names(&pool), ["local", "z", "a", "b"]);
    assert!(!Arc::ptr_eq(&before, &pool.health.members.load_full()));
    let drained = pool.health_snapshot().endpoints[2].administrative_state;
    assert_eq!(
        drained,
        AdministrativeState::Drain,
        "joins in the state they asked for"
    );
}

#[test]
fn names_resolve_to_their_positions_in_small_and_large_pools() {
    for count in [1, 5, 16, 17, 64, 700] {
        let pool = pool(UpstreamAlgorithm::RoundRobin, &[]);
        let joining = (0..count)
            .map(|index| NewMember {
                server: runtime_server(&format!("server-{index}"), 4000, None),
                spec: dynamic(1, 100),
                state: AdministrativeState::Ready,
            })
            .collect();
        assert!(pool.add_servers(joining).iter().all(Result::is_ok));
        let members = pool.health.members.load_full();
        for (position, member) in members.endpoints.iter().enumerate() {
            assert_eq!(members.position(&member.name), Some(position), "{count}");
        }
        assert_eq!(members.position("server-"), None);
        assert_eq!(members.position(&format!("server-{count}")), None);
        assert_eq!(members.position(""), None);
    }
}
