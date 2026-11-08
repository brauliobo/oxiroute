use crate::http::evacuation::{EVACUATED, Fate};

use super::*;

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n";

async fn tracked() -> (Session, HttpRequestContext, TcpStream) {
    let (_, session, mut context, client) = request_preparation_fixture(
        HttpProxyPolicy::default(),
        UpstreamConnectionReuse::Safe,
        GET,
    )
    .await;
    context.register_inflight(&session, None);
    (session, context, client)
}

fn after_first_attempt(context: &mut HttpRequestContext, session: &Session) {
    context.attempted_upstreams.push("node-a".into());
    let cancellation = context.arm_evacuation(session);
    assert!(cancellation.is_some(), "a tracked request can be cancelled");
}

#[tokio::test]
async fn a_moved_attempt_is_neither_a_try_nor_a_retry() {
    let (session, mut context, _client) = tracked().await;
    after_first_attempt(&mut context, &session);
    context.evacuation().expect("handle").evacuate(false, true);

    let error = context
        .cancelled_by_operator()
        .expect("the operator moved the attempt");

    assert!(error.retry());
    assert_eq!(error.etype(), &EVACUATED);
    assert_eq!(context.attempt_count(), 0);
    assert!(context.has_attempted());
    assert_eq!(context.attempted_upstreams, ["node-a"]);
    assert_eq!(context.evacuations, 1);
    assert!(context.selected.is_none());
    assert!(context.retry_server.is_none() && !context.retry_delay_pending);
    assert!(!context.trace.budget_spent.load(Ordering::Relaxed));
    assert_eq!(context.evacuation().expect("handle").redispatches(), 1);
}

#[tokio::test]
async fn a_move_leaves_the_retry_attempts_of_the_route_untouched() {
    let (session, mut context, _client) = tracked().await;
    after_first_attempt(&mut context, &session);
    context.evacuation().expect("handle").evacuate(false, true);
    context.cancelled_by_operator().expect("moved");
    context.attempted_upstreams.push("node-b".into());

    assert_eq!(
        context.attempt_count(),
        1,
        "only the second server is a try"
    );
    assert_eq!(context.tried_servers(), 1);
}

#[tokio::test]
async fn a_killed_attempt_is_not_retried_and_answers_with_the_evacuated_reason() {
    let (session, mut context, _client) = tracked().await;
    context.evacuation_pin = Some(LeftRunning::NotReplayable);
    after_first_attempt(&mut context, &session);
    context.evacuation().expect("handle").evacuate(true, true);

    let error = context
        .cancelled_by_operator()
        .expect("the operator killed the attempt");

    assert!(!error.retry());
    assert_eq!(
        error_reason(&error, &context, &session),
        Some(ErrorReason::Evacuated)
    );
    assert_eq!(failure_reason(&error, &context, &session), "evacuated");
    assert_eq!(ErrorReason::Evacuated.status(), 503);
    assert_eq!(ErrorReason::Evacuated.header().1, "evacuated");
    assert_eq!(context.attempt_count(), 1);
    assert_eq!(context.evacuations, 0);
    assert!(matches!(
        context.trace.pending_outcome,
        Some(AttemptOutcome::Evacuated)
    ));
}

#[tokio::test]
async fn an_attempt_nobody_cancelled_is_left_alone() {
    let (session, mut context, _client) = tracked().await;
    after_first_attempt(&mut context, &session);

    assert!(context.cancelled_by_operator().is_none());
    assert_eq!(context.attempt_count(), 1);
}

#[tokio::test]
async fn only_final_responses_commit_the_request() {
    let (session, mut context, _client) = tracked().await;
    after_first_attempt(&mut context, &session);
    let handle = Arc::clone(context.evacuation().expect("handle"));
    handle.evacuate(false, true);

    assert!(context.commit_response(StatusCode::CONTINUE).is_ok());
    assert!(context.commit_response(StatusCode::EARLY_HINTS).is_ok());
    let taken = context
        .commit_response(StatusCode::OK)
        .expect_err("the operator won");
    assert_eq!(taken.etype(), &EVACUATED);
    assert_eq!(taken.esource(), &ErrorSource::Internal);
}

#[tokio::test]
async fn a_committed_response_stays_where_it_is() {
    let (session, mut context, _client) = tracked().await;
    after_first_attempt(&mut context, &session);

    assert!(context.commit_response(StatusCode::EARLY_HINTS).is_ok());
    assert!(context.commit_response(StatusCode::OK).is_ok());
    let fate = context.evacuation().expect("handle").evacuate(false, true);
    assert_eq!(fate, Some(Fate::LeftRunning(LeftRunning::ResponseStarted)));
}

#[tokio::test]
async fn the_route_decides_whether_the_attempt_can_move() {
    let (session, mut context, _client) = tracked().await;
    context.evacuation_pin = Some(LeftRunning::Upgraded);
    after_first_attempt(&mut context, &session);

    let fate = context.evacuation().expect("handle").evacuate(false, true);

    assert_eq!(fate, Some(Fate::LeftRunning(LeftRunning::Upgraded)));
}

#[tokio::test]
async fn a_truncated_request_body_pins_the_attempt() {
    let (session, mut context, _client) = tracked().await;
    after_first_attempt(&mut context, &session);

    context.pin_evacuation(LeftRunning::BodyNotReplayable);

    let fate = context.evacuation().expect("handle").evacuate(false, true);
    assert_eq!(
        fate,
        Some(Fate::LeftRunning(LeftRunning::BodyNotReplayable))
    );
}

#[tokio::test]
async fn a_moved_request_that_ends_with_an_error_counts_as_failed_on_its_first_server() {
    let (session, mut context, _client) = tracked().await;
    let server = upstream_stats().server("evacuation-failed", "node-a");
    context.trace.stats = Some(Arc::clone(&server));
    after_first_attempt(&mut context, &session);
    context.evacuation().expect("handle").evacuate(false, true);
    context.cancelled_by_operator().expect("moved");

    context.record_failed_evacuations(Some(&Error::new_down(ErrorType::ConnectionClosed)));
    assert_eq!(server.snapshot().evacuated, [0; 4]);
    context.record_failed_evacuations(None);
    assert_eq!(server.snapshot().evacuated, [0; 4]);
    context.record_failed_evacuations(Some(&Error::new_up(ErrorType::ConnectTimedout)));

    let snapshot = server.snapshot();
    assert_eq!(snapshot.evacuated, [0, 0, 0, 1]);
    assert_eq!(
        snapshot.attempts[7], 1,
        "the cancelled attempt is counted as evacuated"
    );
}
