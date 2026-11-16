use std::time::Duration;

use super::*;

const AGE: Duration = Duration::from_millis(5);

fn token(index: u32, generation: u32) -> Token {
    Token { index, generation }
}

fn order() -> Order {
    Order::new(Age::Fixed(AGE))
}

fn served(order: &mut Order, now: Instant) -> Vec<(Token, EventFlags)> {
    std::iter::from_fn(|| order.next(now)).collect()
}

#[test]
fn the_connection_that_became_ready_last_is_served_first() {
    let mut order = order();
    let start = Instant::now();
    (0..3).for_each(|index| order.ready(token(index, 0), EventFlags::IN));

    let tokens = served(&mut order, start)
        .into_iter()
        .map(|(token, _)| token.index)
        .collect::<Vec<_>>();

    assert_eq!(tokens, [2, 1, 0]);
}

#[test]
fn a_connection_ready_twice_is_served_once_with_both_events() {
    let mut order = order();
    order.ready(token(4, 0), EventFlags::IN);
    order.ready(token(5, 0), EventFlags::IN);
    order.ready(token(4, 0), EventFlags::OUT);

    let ready = served(&mut order, Instant::now());

    assert_eq!(ready.len(), 2);
    let four = ready.iter().find(|(token, _)| token.index == 4).unwrap();
    assert_eq!(four.1, EventFlags::IN | EventFlags::OUT);
}

#[test]
fn a_reused_slot_serves_the_newer_connection_only() {
    let mut order = order();
    order.ready(token(7, 0), EventFlags::IN);
    order.ready(token(7, 1), EventFlags::OUT);

    assert_eq!(
        served(&mut order, Instant::now()),
        [(token(7, 1), EventFlags::OUT)]
    );
}

#[test]
fn a_connection_that_has_waited_for_the_age_is_served_before_newer_ones() {
    let mut order = order();
    order.ready(token(0, 0), EventFlags::IN);
    let waited = Instant::now() + AGE;
    order.begin(waited, 0);
    order.ready(token(1, 0), EventFlags::IN);
    order.ready(token(2, 0), EventFlags::IN);

    let first = order.next(waited).expect("a ready connection").0;

    assert_eq!(first.index, 0);
    assert_eq!(order.next(waited).expect("the newest").0.index, 2);
}

#[test]
fn a_served_connection_can_become_ready_again() {
    let mut order = order();
    order.ready(token(3, 0), EventFlags::IN);
    assert!(order.next(Instant::now()).is_some());
    assert!(!order.backlogged());

    order.ready(token(3, 0), EventFlags::OUT);

    assert_eq!(
        served(&mut order, Instant::now()),
        [(token(3, 0), EventFlags::OUT)]
    );
}
