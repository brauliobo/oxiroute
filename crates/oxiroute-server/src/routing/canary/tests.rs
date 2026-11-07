use super::*;

fn run(result: CanaryResult) -> CanaryRun {
    CanaryRun {
        result,
        reason: (result == CanaryResult::Failed).then_some(HealthFailure::ConnectFailed),
        server: Some("a".into()),
        duration: Duration::from_millis(12),
    }
}

#[test]
fn a_new_state_has_no_result_and_failures_count_until_a_success() {
    let state = CanaryState::new(Duration::from_secs(30));
    assert_eq!(state.snapshot().result, None);
    assert!(state.complete(run(CanaryResult::Ok)).is_none());
    assert!(state.complete(run(CanaryResult::Failed)).is_some());
    state.complete(run(CanaryResult::Failed));
    let failing = state.snapshot();
    assert_eq!(failing.result, Some(CanaryResult::Failed));
    assert_eq!(failing.reason, Some(HealthFailure::ConnectFailed));
    assert_eq!((failing.consecutive_failures, failing.duration_ms), (2, 12));
    assert!(failing.last_success_unix_ms.is_some());
    assert_eq!(failing.interval_ms, 30_000);
    let flipped = state.complete(run(CanaryResult::Slow));
    assert_eq!(flipped, Some((CanaryResult::Failed, CanaryResult::Slow)));
    let healed = state.snapshot();
    assert_eq!(healed.consecutive_failures, 0);
    assert_eq!(healed.runs["failed"], 2);
}

#[test]
fn busy_runs_are_counted_but_change_nothing_else() {
    let state = CanaryState::new(Duration::from_secs(30));
    state.complete(run(CanaryResult::Failed));
    assert!(state.complete(run(CanaryResult::Busy)).is_none());
    let snapshot = state.snapshot();
    assert_eq!(snapshot.result, Some(CanaryResult::Failed));
    assert_eq!(snapshot.consecutive_failures, 1);
    assert_eq!(snapshot.runs["busy"], 1);
    assert!(snapshot.last_attempt_unix_ms.is_some());
}

#[test]
fn an_unavailable_pool_is_not_a_canary_failure() {
    let state = CanaryState::new(Duration::from_secs(30));
    state.complete(run(CanaryResult::Ok));
    state.complete(run(CanaryResult::Unavailable));
    let snapshot = state.snapshot();
    assert_eq!(snapshot.result, Some(CanaryResult::Unavailable));
    assert_eq!(snapshot.consecutive_failures, 0);
}
