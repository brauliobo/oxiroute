use std::sync::{Arc, Barrier};

use futures_util::FutureExt as _;

use super::*;

fn armed(unmovable: Option<LeftRunning>) -> Arc<Evacuation> {
    let evacuation = Arc::new(Evacuation::new());
    evacuation.begin_attempt(unmovable);
    evacuation
}

#[test]
fn a_request_without_an_attempt_is_not_a_candidate() {
    let evacuation = Evacuation::new();
    assert_eq!(evacuation.evacuate(true, true), None);
    assert_eq!(evacuation.settle(), None);
}

#[test]
fn a_movable_attempt_is_taken_once_and_settles_as_a_move() {
    let evacuation = armed(None);
    assert_eq!(evacuation.evacuate(false, true), Some(Fate::Redispatched));
    assert_eq!(evacuation.evacuate(false, true), None);
    assert_eq!(evacuation.evacuate(true, true), None);
    assert!(!evacuation.commit_response());
    assert_eq!(evacuation.settle(), Some(Cancellation::Move));
    assert_eq!(evacuation.redispatches(), 1);
    assert_eq!(evacuation.settle(), None);
}

#[test]
fn a_movable_attempt_without_a_destination_is_left_running_even_when_forced() {
    let evacuation = armed(None);
    for force in [false, true] {
        assert_eq!(
            evacuation.evacuate(force, false),
            Some(Fate::LeftRunning(LeftRunning::NoDestination))
        );
    }
    assert!(evacuation.commit_response());
}

#[test]
fn pinned_attempts_are_left_running_unless_forced() {
    for reason in [
        LeftRunning::NotReplayable,
        LeftRunning::BodyNotReplayable,
        LeftRunning::Upgraded,
        LeftRunning::LimitReached,
        LeftRunning::ResponseStarted,
    ] {
        let evacuation = armed(Some(reason));
        assert_eq!(
            evacuation.evacuate(false, true),
            Some(Fate::LeftRunning(reason))
        );
        assert_eq!(evacuation.evacuate(true, true), Some(Fate::Forced));
        assert_eq!(evacuation.evacuate(true, true), None);
        assert!(!evacuation.commit_response());
        assert_eq!(evacuation.settle(), Some(Cancellation::Kill));
        assert_eq!(evacuation.redispatches(), 0);
    }
}

#[test]
fn committing_the_response_pins_the_request() {
    let evacuation = armed(None);
    assert!(evacuation.commit_response());
    assert!(evacuation.commit_response());
    assert_eq!(
        evacuation.evacuate(false, true),
        Some(Fate::LeftRunning(LeftRunning::ResponseStarted))
    );
    assert_eq!(evacuation.settle(), None);
}

#[test]
fn a_truncated_body_pins_only_a_movable_attempt() {
    let evacuation = armed(None);
    evacuation.pin(LeftRunning::BodyNotReplayable);
    assert_eq!(
        evacuation.evacuate(false, true),
        Some(Fate::LeftRunning(LeftRunning::BodyNotReplayable))
    );
    let taken = armed(None);
    taken.evacuate(false, true);
    taken.pin(LeftRunning::BodyNotReplayable);
    assert_eq!(taken.settle(), Some(Cancellation::Move));
}

#[test]
fn the_fourth_move_is_refused() {
    let evacuation = Arc::new(Evacuation::new());
    for _ in 0..MAX_EVACUATIONS {
        evacuation.begin_attempt(None);
        assert_eq!(evacuation.evacuate(false, true), Some(Fate::Redispatched));
        assert_eq!(evacuation.settle(), Some(Cancellation::Move));
    }
    evacuation.begin_attempt(None);
    assert_eq!(
        evacuation.evacuate(false, true),
        Some(Fate::LeftRunning(LeftRunning::LimitReached))
    );
}

#[test]
fn a_request_policy_reason_wins_over_the_move_limit() {
    let evacuation = Evacuation::new();
    evacuation
        .redispatched
        .store(MAX_EVACUATIONS, Ordering::Release);
    evacuation.begin_attempt(Some(LeftRunning::Upgraded));
    assert_eq!(
        evacuation.evacuate(false, true),
        Some(Fate::LeftRunning(LeftRunning::Upgraded))
    );
}

#[test]
fn the_end_of_an_attempt_makes_the_request_idle_again() {
    let evacuation = armed(None);
    assert_eq!(evacuation.settle(), None);
    assert_eq!(evacuation.evacuate(true, true), None);
}

#[test]
fn the_proxy_and_the_operator_never_both_win() {
    for _ in 0..2_000 {
        let evacuation = armed(None);
        let barrier = Arc::new(Barrier::new(2));
        let operator = {
            let (evacuation, barrier) = (Arc::clone(&evacuation), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                evacuation.evacuate(false, true)
            })
        };
        barrier.wait();
        let committed = evacuation.commit_response();
        match operator.join().expect("operator") {
            Some(Fate::Redispatched) => assert!(!committed),
            Some(Fate::LeftRunning(LeftRunning::ResponseStarted)) => assert!(committed),
            other => panic!("unexpected fate {other:?}"),
        }
    }
}

#[test]
fn concurrent_operators_move_a_request_once() {
    for _ in 0..500 {
        let evacuation = armed(None);
        let barrier = Arc::new(Barrier::new(4));
        let operators = (0..4)
            .map(|_| {
                let (evacuation, barrier) = (Arc::clone(&evacuation), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    evacuation.evacuate(false, true)
                })
            })
            .collect::<Vec<_>>();
        let fates = operators
            .into_iter()
            .map(|operator| operator.join().expect("operator"))
            .collect::<Vec<_>>();
        assert_eq!(
            fates
                .iter()
                .filter(|fate| **fate == Some(Fate::Redispatched))
                .count(),
            1
        );
        assert!(
            fates
                .iter()
                .all(|fate| matches!(fate, None | Some(Fate::Redispatched)))
        );
    }
}

#[test]
fn the_cancellation_resolves_with_the_evacuation_error_once_the_operator_acts() {
    let evacuation = armed(None);
    let mut cancellation = evacuation.cancellation();
    assert!(cancellation.as_mut().now_or_never().is_none());
    evacuation.evacuate(false, true);
    let error = cancellation.now_or_never().expect("cancelled");
    assert!(is_evacuated(&error));
    assert_eq!(error.esource(), &pingora::ErrorSource::Internal);
}

#[test]
fn a_cancellation_requested_before_the_future_runs_is_not_lost() {
    let evacuation = armed(None);
    evacuation.evacuate(true, true);
    assert!(evacuation.cancellation().now_or_never().is_some());
}

#[test]
fn a_stale_wakeup_does_not_cancel_the_next_attempt() {
    let evacuation = armed(None);
    evacuation.evacuate(false, true);
    assert_eq!(evacuation.settle(), Some(Cancellation::Move));
    evacuation.begin_attempt(None);
    assert!(evacuation.cancellation().now_or_never().is_none());
}

#[test]
fn reports_count_per_fate_merge_and_serialize_for_the_api() {
    let mut report = EvacuationReport::default();
    report.record(Fate::Redispatched);
    report.record(Fate::Forced);
    report.record(Fate::LeftRunning(LeftRunning::NotReplayable));
    let mut other = EvacuationReport::default();
    other.record(Fate::Redispatched);
    other.record(Fate::LeftRunning(LeftRunning::NotReplayable));
    other.record(Fate::LeftRunning(LeftRunning::NoDestination));
    report.merge(other);

    assert_eq!(report.left_running_total(), 3);
    assert_eq!(
        serde_json::to_value(&report).expect("report json"),
        serde_json::json!({
            "cancelledAndRedispatched": 2,
            "forced": 1,
            "leftRunning": { "not_replayable": 2, "no_destination": 1 },
        })
    );
}

#[test]
fn unmovable_reasons_follow_the_route_and_the_protocol() {
    assert_eq!(
        LeftRunning::unmovable(true, true),
        Some(LeftRunning::Upgraded)
    );
    assert_eq!(
        LeftRunning::unmovable(true, false),
        Some(LeftRunning::Upgraded)
    );
    assert_eq!(
        LeftRunning::unmovable(false, false),
        Some(LeftRunning::NotReplayable)
    );
    assert_eq!(LeftRunning::unmovable(false, true), None);
}

#[test]
fn every_state_survives_its_encoding() {
    let pinned = LeftRunning::ALL.map(State::Pinned);
    for state in [State::Idle, State::Movable, State::Moving, State::Killing]
        .into_iter()
        .chain(pinned)
    {
        assert_eq!(State::decode(state.encode()), state);
    }
}
