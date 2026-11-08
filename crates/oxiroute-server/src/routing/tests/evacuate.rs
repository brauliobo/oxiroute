use super::*;
use crate::{
    LeftRunning,
    http::{
        evacuation::Cancellation,
        inflight::{InflightArrival, InflightGuard, registry},
    },
    observability::upstream_stats::upstream_stats,
};

fn farm(name: &str, capacity: Option<u64>, queue: Option<Duration>) -> RoundRobinPool {
    RoundRobinPool::new_named_servers_resilient(
        name.into(),
        ["node-a", "node-b"]
            .into_iter()
            .zip(3000..)
            .map(|(server, port)| runtime_server(server, port, capacity)),
        UpstreamAlgorithm::First,
        Some(HealthStartup::Healthy),
        queue,
        PassiveFailurePolicy::default(),
        PoolResiliencePolicy::default(),
    )
    .expect("pool")
}

fn attempt(pool: &str, server: &str, unmovable: Option<LeftRunning>) -> InflightGuard {
    let guard = registry().register(InflightArrival {
        request_id: format!("{pool}-{server}-{unmovable:?}"),
        listener: "edge".into(),
        method: "GET".into(),
        host: None,
        path: "/".into(),
        client_ip: None,
        deadline: Duration::from_secs(30),
        access_log: None,
    });
    guard.set_pool(pool);
    guard.set_server(server);
    guard.evacuation().begin_attempt(unmovable);
    guard
}

#[test]
fn evacuating_a_server_drains_it_and_moves_its_requests() {
    let pool = farm("evacuate-moves", None, None);
    let movable = attempt("evacuate-moves", "node-a", None);
    let pinned = attempt("evacuate-moves", "node-a", Some(LeftRunning::NotReplayable));
    let other = attempt("evacuate-moves", "node-b", None);

    let report = pool.evacuate_server("node-a", false).expect("evacuate");

    assert_eq!(report.redispatched, 1);
    assert_eq!(
        report.left_running.get(&LeftRunning::NotReplayable),
        Some(&1)
    );
    let states = pool.administrative_states();
    assert_eq!(states.len(), 1);
    assert_eq!(
        (states[0].server.as_str(), states[0].administrative),
        ("node-a", AdministrativeState::Drain)
    );
    assert_eq!(pool.select().expect("lease").server_name(), "node-b");
    assert!(movable.evacuation().settle().is_some());
    assert!(pinned.evacuation().settle().is_none());
    assert!(other.evacuation().settle().is_none());
    let counted = upstream_stats()
        .server("evacuate-moves", "node-a")
        .snapshot()
        .evacuated;
    assert_eq!(counted, [1, 1, 0, 0]);
}

#[test]
fn forcing_kills_what_cannot_move_and_counts_it() {
    let pool = farm("evacuate-forced", None, None);
    let streaming = attempt(
        "evacuate-forced",
        "node-a",
        Some(LeftRunning::ResponseStarted),
    );

    let report = pool.evacuate_server("node-a", true).expect("evacuate");

    assert_eq!(
        (
            report.redispatched,
            report.forced,
            report.left_running_total()
        ),
        (0, 1, 0)
    );
    assert!(streaming.evacuation().settle().is_some());
    let counted = upstream_stats()
        .server("evacuate-forced", "node-a")
        .snapshot()
        .evacuated;
    assert_eq!(counted, [0, 0, 1, 0]);
}

#[test]
fn an_unknown_server_is_refused_before_anything_changes() {
    let pool = farm("evacuate-unknown", None, None);
    assert_eq!(
        pool.evacuate_server("gone", true),
        Err(PoolAdminError::UnknownServer("gone".into()))
    );
    assert!(pool.administrative_states().is_empty());
}

#[test]
fn the_last_server_has_no_destination() {
    let pool = farm("evacuate-last", None, None);
    pool.set_server_administrative_state("node-b", AdministrativeState::Maintenance)
        .expect("maintenance");
    let request = attempt("evacuate-last", "node-a", None);

    let report = pool.evacuate_server("node-a", false).expect("evacuate");

    assert_eq!(report.redispatched, 0);
    assert_eq!(
        report.left_running.get(&LeftRunning::NoDestination),
        Some(&1)
    );
    assert!(request.evacuation().settle().is_none());
}

#[test]
fn a_full_destination_counts_only_when_the_pool_queues() {
    for (queue, destination) in [(None, false), (Some(Duration::from_secs(5)), true)] {
        let name = format!("evacuate-full-{destination}");
        let pool = farm(&name, Some(1), queue);
        let _busy = pool
            .select_server_excluding(&["node-a".into()])
            .lease
            .expect("node-b lease");
        let request = attempt(&name, "node-a", None);

        let report = pool.evacuate_server("node-a", false).expect("evacuate");

        assert_eq!(
            report.redispatched,
            u64::from(destination),
            "queue {queue:?}"
        );
        assert_eq!(request.evacuation().settle().is_some(), destination);
    }
}

#[test]
fn unselectable_servers_are_not_destinations() {
    let pool = farm("evacuate-down", None, None);
    pool.set_server_health_override("node-b", HealthOverride::Down)
        .expect("down");
    let request = attempt("evacuate-down", "node-a", None);

    let report = pool.evacuate_server("node-a", false).expect("evacuate");

    assert_eq!(
        report.left_running.get(&LeftRunning::NoDestination),
        Some(&1)
    );
    assert!(request.evacuation().settle().is_none());
}

#[test]
fn cancelling_without_draining_leaves_the_state_to_the_caller() {
    let pool = farm("evacuate-cancel-only", None, None);
    let request = attempt("evacuate-cancel-only", "node-a", None);

    let report = pool
        .cancel_server_requests("node-a", false)
        .expect("cancel");

    assert_eq!(report.redispatched, 1, "the other server is a destination");
    assert!(pool.administrative_states().is_empty());
    assert!(request.evacuation().settle().is_some());
}

#[test]
fn forcing_kills_the_requests_of_the_last_server() {
    let pool = farm("evacuate-last-forced", None, None);
    pool.set_server_administrative_state("node-b", AdministrativeState::Maintenance)
        .expect("maintenance");
    let request = attempt("evacuate-last-forced", "node-a", None);

    let report = pool.evacuate_server("node-a", true).expect("evacuate");

    assert_eq!((report.redispatched, report.forced), (0, 1));
    assert_eq!(request.evacuation().settle(), Some(Cancellation::Kill));
}

#[test]
fn evacuating_every_server_drains_the_pool_before_cancelling_so_nothing_hops() {
    let pool = farm("evacuate-all", None, None);
    let on_a = attempt("evacuate-all", "node-a", None);
    let on_b = attempt("evacuate-all", "node-b", None);
    let pinned = attempt("evacuate-all", "node-b", Some(LeftRunning::NotReplayable));

    let report = pool.evacuate_all_servers(false);

    assert_eq!((report.redispatched, report.forced), (0, 0));
    assert_eq!(
        report.left_running.get(&LeftRunning::NoDestination),
        Some(&2)
    );
    assert_eq!(
        report.left_running.get(&LeftRunning::NotReplayable),
        Some(&1)
    );
    let states = pool.administrative_states();
    assert_eq!(states.len(), 2);
    assert!(
        states
            .iter()
            .all(|state| state.administrative == AdministrativeState::Drain)
    );
    assert!(pool.select().is_none());
    assert!(
        [&on_a, &on_b, &pinned]
            .iter()
            .all(|guard| guard.evacuation().settle().is_none())
    );
    let counted = |server| {
        upstream_stats()
            .server("evacuate-all", server)
            .snapshot()
            .evacuated
    };
    assert_eq!(counted("node-a"), [0, 1, 0, 0]);
    assert_eq!(counted("node-b"), [0, 2, 0, 0]);
}

#[test]
fn forcing_every_server_cuts_all_their_requests() {
    let pool = farm("evacuate-all-forced", None, None);
    let on_a = attempt("evacuate-all-forced", "node-a", None);
    let on_b = attempt(
        "evacuate-all-forced",
        "node-b",
        Some(LeftRunning::ResponseStarted),
    );

    let report = pool.evacuate_all_servers(true);

    assert_eq!(
        (
            report.redispatched,
            report.forced,
            report.left_running_total()
        ),
        (0, 2, 0)
    );
    assert_eq!(on_a.evacuation().settle(), Some(Cancellation::Kill));
    assert_eq!(on_b.evacuation().settle(), Some(Cancellation::Kill));
    let counted = |server| {
        upstream_stats()
            .server("evacuate-all-forced", server)
            .snapshot()
            .evacuated
    };
    assert_eq!(
        (counted("node-a"), counted("node-b")),
        ([0, 0, 1, 0], [0, 0, 1, 0])
    );
}
