use super::*;

#[test]
fn replay_safety_requires_the_explicit_buffered_all_methods_policy() {
    let default_policy = ProxyPolicyPlan::compile(&HttpProxyPolicy::default());
    assert!(default_policy.request_is_replay_safe(&Method::GET, true, false));
    assert!(!default_policy.request_is_replay_safe(&Method::GET, false, true));
    assert!(!default_policy.request_is_replay_safe(&Method::POST, true, true));

    let replay_policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        retry: oxiroute_config::HttpRetryPolicy {
            method_safety: HttpRetryMethodSafety::All,
            body_safety: HttpRetryBodySafety::Buffered,
            ..oxiroute_config::HttpRetryPolicy::default()
        },
        ..HttpProxyPolicy::default()
    });
    assert!(replay_policy.request_is_replay_safe(&Method::POST, true, false));
    assert!(!replay_policy.request_is_replay_safe(&Method::POST, false, false));
    assert!(replay_policy.request_is_replay_safe(&Method::POST, false, true));
}
