use oxiroute_config::HttpRouteAction;

use super::common::{assert_blocker, assert_has_provenance, import_fixture};

#[test]
fn bare_redispatch_lowers_to_delayed_same_server_retries_and_final_redispatch() {
    let source = b"defaults web
  mode http
  retries 3
  option redispatch
  timeout connect 600s
  timeout server 600s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
  server app2 127.0.0.1:3001
";
    let lowered = import_fixture("redispatch.cfg", source);
    let config = lowered
        .value()
        .validated()
        .unwrap_or_else(|| {
            panic!(
                "bare redispatch did not finalize: {:#?}",
                lowered.diagnostics()
            )
        })
        .as_draft();
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route")
    };

    assert_eq!(policy.retry.max_retries, 3);
    assert_eq!(
        policy.retry.target,
        oxiroute_config::HttpRetryTarget::SameServer
    );
    assert_eq!(policy.retry.delay_ms, 1_000);
    assert!(policy.retry.final_redispatch);
    assert_eq!(
        policy.retry.triggers,
        [
            oxiroute_config::HttpRetryTrigger::ConnectFailure,
            oxiroute_config::HttpRetryTrigger::ConnectTimeout,
        ]
    );
}

#[test]
fn redispatch_interval_forms_remain_blocking() {
    let source = b"defaults web
  mode http
  retries 3
  option redispatch 2
  timeout connect 5s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("redispatch-interval.cfg", source);

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_blocker(lowered.diagnostics(), "redispatch interval forms");
}

#[test]
fn positive_http_retries_finalize_for_unrestricted_methods_as_pre_send_connection_retries() {
    let source = b"defaults web
  mode http
  retries 1
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  maxconn 100
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("positive-http-retries.cfg", source);

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some(),
        "{:?}",
        lowered.diagnostics()
    );
    let HttpRouteAction::Proxy { policy, .. } = &lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .unwrap()
        .http_services[0]
        .routes[0]
        .action
    else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 1);
}

#[test]
fn passive_server_policy_and_safe_retry_on_subset_lower_exactly() {
    let source = b"defaults web
  mode http
  retries 2
  retry-on conn-failure conn-refused
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  maxconn 100
  default_backend app
backend app
  balance roundrobin
  default-server check observe layer7 error-limit 4 on-error mark-down
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("passive-retry-subset.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized config");
    let pool = &config.upstream_pools[0];
    let passive = pool.passive_health.as_ref().expect("passive policy");
    assert_eq!(passive.observe, oxiroute_config::PassiveObserve::Layer7);
    assert_eq!(passive.on_error, oxiroute_config::PassiveOnError::MarkDown);
    assert_eq!(passive.error_limit, 4);
    assert!(passive.mark_down);
    assert_has_provenance(lowered.value(), "/upstream_pools/0/passive_health/observe");
    assert_has_provenance(
        lowered.value(),
        "/upstream_pools/0/passive_health/error_limit",
    );
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 2);
    assert_eq!(
        policy.retry.triggers,
        vec![
            oxiroute_config::HttpRetryTrigger::ConnectFailure,
            oxiroute_config::HttpRetryTrigger::ConnectTimeout,
        ]
    );
}

#[test]
fn retry_on_status_and_response_forms_lower_to_the_canonical_policy() {
    let source = b"defaults web
  mode http
  retries 2
  retry-on conn-failure empty-response response-timeout junk-response
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
  server app2 127.0.0.1:3001
";
    let trigger = import_fixture("retry-on-triggers.cfg", source);
    let config = trigger
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("trigger config");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert_eq!(
        policy.retry.triggers,
        [
            oxiroute_config::HttpRetryTrigger::ConnectFailure,
            oxiroute_config::HttpRetryTrigger::ConnectTimeout,
            oxiroute_config::HttpRetryTrigger::EmptyResponse,
            oxiroute_config::HttpRetryTrigger::ResponseTimeout,
            oxiroute_config::HttpRetryTrigger::JunkResponse,
        ]
    );
    assert!(policy.retry.response_statuses.is_empty());

    let status_source = String::from_utf8(source.to_vec())
        .expect("trigger source")
        .replace(
            "retry-on conn-failure empty-response response-timeout junk-response",
            "retry-on 500 502 504",
        );
    let status = import_fixture("retry-on-statuses.cfg", status_source.as_bytes());
    let config = status
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("status config");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert!(policy.retry.triggers.is_empty());
    assert_eq!(policy.retry.response_statuses, [500, 502, 504]);
    assert_has_provenance(
        status.value(),
        "/http_services/0/routes/0/action/policy/retry/response_statuses",
    );

    let all_source = status_source.replace("retry-on 500 502 504", "retry-on all");
    let all = import_fixture("retry-on-all.cfg", all_source.as_bytes());
    let config = all
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("all config");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.triggers.len(), 5);
    assert_eq!(policy.retry.response_statuses.len(), 100);
}

#[test]
fn unsupported_retry_on_semantics_remain_blocking() {
    let source = b"defaults web
  mode http
  retries 1
  retry-on 0rtt-rejected
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("unsupported-retry-on.cfg", source);

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_blocker(
        lowered.diagnostics(),
        "HAProxy retry-on form is not represented by supported canonical retry policy",
    );
}

#[test]
fn duplicate_passive_server_options_remain_blocking() {
    let source = b"defaults web
  mode http
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000 observe layer7 observe layer7
";
    let lowered = import_fixture("duplicate-passive-option.cfg", source);

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(lowered.has_errors());
}
