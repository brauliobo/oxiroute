use super::*;

#[test]
fn replay_safety_requires_the_explicit_buffered_all_methods_policy() {
    let default_policy = ProxyPolicyPlan::compile(&HttpProxyPolicy::default());
    assert!(default_policy.request_is_replay_safe(&Method::GET, &HeaderMap::new(), true, false));
    assert!(!default_policy.request_is_replay_safe(&Method::GET, &HeaderMap::new(), false, true));
    assert!(!default_policy.request_is_replay_safe(&Method::POST, &HeaderMap::new(), true, true));

    let replay_policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        retry: oxiroute_config::HttpRetryPolicy {
            method_safety: HttpRetryMethodSafety::All,
            body_safety: HttpRetryBodySafety::Buffered,
            ..oxiroute_config::HttpRetryPolicy::default()
        },
        ..HttpProxyPolicy::default()
    });
    assert!(replay_policy.request_is_replay_safe(&Method::POST, &HeaderMap::new(), true, false));
    assert!(!replay_policy.request_is_replay_safe(&Method::POST, &HeaderMap::new(), false, false));
    assert!(replay_policy.request_is_replay_safe(&Method::POST, &HeaderMap::new(), false, true));
}

fn idempotent_policy() -> ProxyPolicyPlan {
    ProxyPolicyPlan::compile(&HttpProxyPolicy {
        retry: oxiroute_config::HttpRetryPolicy {
            method_safety: HttpRetryMethodSafety::All,
            body_safety: HttpRetryBodySafety::Buffered,
            idempotency_header: Some("idempotency-key".into()),
            ..oxiroute_config::HttpRetryPolicy::default()
        },
        ..HttpProxyPolicy::default()
    })
}

#[test]
fn the_idempotency_header_gates_only_methods_other_than_get_and_head() {
    let policy = idempotent_policy();
    let keyed = HeaderMap::from_iter([(
        HeaderName::from_static("idempotency-key"),
        HeaderValue::from_static("job-1"),
    )]);
    let empty = HeaderMap::from_iter([(
        HeaderName::from_static("idempotency-key"),
        HeaderValue::from_static(""),
    )]);
    assert!(policy.request_is_replay_safe(&Method::POST, &keyed, false, true));
    assert!(!policy.request_is_replay_safe(&Method::POST, &HeaderMap::new(), false, true));
    assert!(!policy.request_is_replay_safe(&Method::POST, &empty, false, true));
    assert!(policy.request_is_replay_safe(&Method::GET, &HeaderMap::new(), true, false));
    assert!(policy.request_is_replay_safe(&Method::HEAD, &HeaderMap::new(), true, false));
}

#[test]
fn jitter_adds_up_to_the_configured_share_of_the_delay() {
    let base = Duration::from_secs(1);
    assert_eq!(jittered_delay(base, 0, 0.9), base);
    assert_eq!(jittered_delay(base, 50, 0.0), base);
    assert_eq!(jittered_delay(base, 50, 0.5), Duration::from_millis(1_250));
    assert_eq!(jittered_delay(base, 100, 1.0), Duration::from_secs(2));
    for _ in 0..100 {
        let delay = jittered_delay(base, 30, random_unit());
        assert!(
            delay >= base && delay <= Duration::from_millis(1_300),
            "{delay:?}"
        );
    }
}

#[test]
fn the_retry_budget_caps_the_rate_per_second() {
    let budget = RetryBudget::new(oxiroute_config::HttpRetryBudget {
        per_second: Some(2),
        ratio_percent: None,
    });
    let start = Instant::now();
    assert!(budget.try_spend(start));
    assert!(budget.try_spend(start));
    assert!(!budget.try_spend(start));
    assert!(budget.try_spend(start + Duration::from_millis(1_100)));
}

#[test]
fn the_retry_budget_caps_the_ratio_of_recent_requests() {
    let budget = RetryBudget::new(oxiroute_config::HttpRetryBudget {
        per_second: None,
        ratio_percent: Some(10),
    });
    let start = Instant::now();
    assert!(!budget.try_spend(start), "no request, no retry");
    let mut spent = 0;
    for request in 0_u32..100 {
        budget.record_request(start);
        spent += u32::from(budget.try_spend(start));
        assert!(spent <= (request + 1).div_ceil(10), "request {request}");
    }
    assert_eq!(spent, 10);
    let later = start + BUDGET_RATIO_WINDOW;
    budget.record_request(later);
    assert!(budget.try_spend(later), "the ratio window rolls over");
}
