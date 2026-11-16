use std::time::Duration;

use oxiroute_epoll_h1::{Age, Config, Timeouts};

use crate::{
    common::{HOUR, aged, plain, recorded},
    support::{Fixture, Knobs, Wire},
};

const PAUSE: Duration = Duration::from_millis(300);

fn started(config: Config, pause: Duration) -> Fixture {
    let knobs = Knobs {
        pause_in_route: Some(pause),
        ..recorded()
    };
    Fixture::with(config, Timeouts::default(), knobs)
}

fn warmed(fixture: &Fixture, count: usize) -> Vec<Wire> {
    (0..count)
        .map(|_| {
            let mut wire = fixture.wire();
            wire.get("/warm");
            assert_eq!(wire.reply().status, 200);
            wire
        })
        .collect()
}

fn routed_after(fixture: &Fixture, first: &str) -> Vec<String> {
    let routed = fixture.service.routed.lock().expect("routed").clone();
    let start = routed
        .iter()
        .position(|target| target == first)
        .expect("the first request");
    routed[start..].to_vec()
}

fn replies(wires: &mut [Wire]) {
    wires
        .iter_mut()
        .for_each(|wire| assert_eq!(wire.reply().status, 200));
}

fn order_of_three(config: Config) -> Vec<String> {
    let fixture = started(config, PAUSE);
    let mut wires = warmed(&fixture, 3);
    let mut gate = fixture.wire();
    gate.get("/pause");
    fixture.paused.recv().expect("the thread entered the route");
    for (wire, path) in wires.iter_mut().zip(["/b", "/c", "/d"]) {
        wire.get(path);
    }
    assert_eq!(gate.reply().status, 200);
    replies(&mut wires);
    routed_after(&fixture, "/pause")
}

#[test]
fn without_scheduling_connections_that_became_ready_together_are_served_in_arrival_order() {
    assert_eq!(order_of_three(plain(1)), ["/pause", "/b", "/c", "/d"]);
}

#[test]
fn with_scheduling_the_connection_that_became_ready_last_is_served_first() {
    assert_eq!(
        order_of_three(aged(1, Age::Fixed(HOUR))),
        ["/pause", "/d", "/c", "/b"]
    );
}

const CONNECTIONS: usize = 48;

fn order_under_backlog(config: Config) -> Vec<usize> {
    let fixture = started(config, Duration::from_millis(10));
    let mut wires = warmed(&fixture, CONNECTIONS);
    for (id, wire) in wires.iter_mut().enumerate() {
        wire.get(&named(id));
        if id == 0 {
            fixture.paused.recv().expect("the thread entered the route");
        }
    }
    replies(&mut wires);
    routed_after(&fixture, "/pause-0")
        .iter()
        .map(|target| {
            target
                .trim_start_matches("/pause-")
                .parse()
                .expect("a connection")
        })
        .collect()
}

fn named(id: usize) -> String {
    format!("/pause-{id}")
}

#[test]
fn a_backlog_with_no_age_is_served_newest_first_down_to_the_oldest() {
    let order = order_under_backlog(aged(1, Age::Fixed(HOUR)));

    let expected = (1..CONNECTIONS).rev().collect::<Vec<_>>();
    assert_eq!(order[0], 0);
    assert_eq!(order[1..], expected, "{order:?}");
}

#[test]
fn the_oldest_ready_connection_is_served_first_once_it_has_waited_for_the_age() {
    let order = order_under_backlog(aged(1, Age::Fixed(Duration::from_millis(40))));

    assert_eq!(order[0], 0);
    let oldest = order
        .iter()
        .position(|id| *id == 1)
        .expect("the oldest was served");
    let newest_first = &order[1..oldest];
    assert!(newest_first.len() >= 16, "{order:?}");
    let expected = (CONNECTIONS - newest_first.len()..CONNECTIONS)
        .rev()
        .collect::<Vec<_>>();
    assert_eq!(newest_first, expected, "{order:?}");
    let left = CONNECTIONS - 1 - newest_first.len();
    assert_eq!(order[oldest..], (1..=left).collect::<Vec<_>>(), "{order:?}");
}

#[test]
fn the_age_changes_only_the_order_every_request_is_answered_once() {
    let mut order = order_under_backlog(aged(1, Age::Fixed(Duration::from_millis(40))));

    order.sort_unstable();
    assert_eq!(order, (0..CONNECTIONS).collect::<Vec<_>>());
}
