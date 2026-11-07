use super::*;
use crate::model::{HttpRetryBodySafety, HttpRetryMethodSafety, HttpRoutePolicy};

fn buffered_retry() -> HttpRetryPolicy {
    HttpRetryPolicy {
        method_safety: HttpRetryMethodSafety::All,
        body_safety: HttpRetryBodySafety::Buffered,
        ..HttpRetryPolicy::default()
    }
}

#[test]
fn buffered_retries_require_the_complete_explicit_opt_in() {
    let mut retry = HttpRetryPolicy {
        method_safety: HttpRetryMethodSafety::All,
        ..HttpRetryPolicy::default()
    };
    assert!(validate_retry("service", 0, &mut retry, HttpRoutePolicy::default()).is_err());

    let mut retry = HttpRetryPolicy {
        body_safety: HttpRetryBodySafety::Buffered,
        ..HttpRetryPolicy::default()
    };
    assert!(validate_retry("service", 0, &mut retry, HttpRoutePolicy::default()).is_err());
}

#[test]
fn buffered_retries_require_bounded_request_buffering() {
    let mut retry = buffered_retry();
    assert!(validate_retry("service", 0, &mut retry, HttpRoutePolicy::default()).is_err());

    let route = HttpRoutePolicy {
        request_buffering: true,
        max_request_body_bytes: None,
        ..HttpRoutePolicy::default()
    };
    assert!(validate_retry("service", 0, &mut retry, route).is_err());

    let mut retry = buffered_retry();
    let route = HttpRoutePolicy {
        request_buffering: true,
        ..HttpRoutePolicy::default()
    };
    validate_retry("service", 0, &mut retry, route).expect("explicit buffered retry opt-in");
}
