use oxiroute_config::{HttpHostSelector, HttpPathSelector, HttpRouteAction};

use super::common::{assert_has_provenance, diagnostic_contains, import_fixture};

const ACL_CONJUNCTION: &[u8] = include_bytes!("../../fixtures/haproxy/acl-conjunction.cfg");

#[test]
fn imported_http_services_disable_automatic_response_headers_in_canonical_rendering() {
    let lowered = import_fixture("path-routing.cfg", routing_fixture().as_bytes());
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("HAProxy HTTP config");

    assert!(
        config
            .http_services
            .iter()
            .all(|service| !service.automatic_response_headers)
    );
    let source = serde_json::to_string(config).expect("serialized HAProxy import");
    assert!(source.contains("\"automatic_response_headers\":false"));
    assert!(!source.contains("\"automatic_response_headers\":true"));
}

#[test]
fn raw_path_prefix_acl_is_not_widened_or_narrowed_to_segment_matching() {
    let lowered = import_fixture("path-routing.cfg", routing_fixture().as_bytes());

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("raw-prefix config");
    assert!(matches!(
        config.http_services[0].routes[0].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/api"
    ));
}

#[test]
fn host_header_acl_is_blocked_because_canonical_host_matching_normalizes_ports() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  maxconn 100
  acl app_host hdr(host) app.example
  use_backend app if app_host
  default_backend fallback
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
backend fallback
  balance roundrobin
  server fallback1 127.0.0.1:3002
";
    let lowered = import_fixture("host-routing.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("authority config");
    assert!(matches!(
        config.http_services[0].routes[0].host,
        Some(HttpHostSelector::ExactAuthority { ref value }) if value == "app.example"
    ));
}

#[test]
fn case_insensitive_host_acl_lowers_to_exact_authority_without_port_widening() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  maxconn 100
  acl app_host hdr(host) -i app.example
  use_backend app if app_host
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
";
    let lowered = import_fixture("case-insensitive-host.cfg", source);

    let config = lowered
        .value()
        .validated()
        .unwrap_or_else(|| {
            panic!(
                "case-insensitive authority did not finalize: {:#?}",
                lowered.diagnostics()
            )
        })
        .as_draft();
    assert!(matches!(
        config.http_services[0].routes[0].host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "app.example"
    ));
    assert!(matches!(
        config.http_services[0].routes[1].action,
        HttpRouteAction::FixedResponse { status: 503, .. }
    ));
}

#[test]
fn positive_host_and_path_acl_conjunction_lowers_with_both_matchers_and_provenance() {
    let imported = import_fixture("acl-conjunction.cfg", ACL_CONJUNCTION);
    let candidate = imported.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("host/path ACL conjunction config");
    let service = &config.http_services[0];

    assert!(
        imported.diagnostics().is_empty(),
        "{:?}",
        imported.diagnostics()
    );
    assert!(matches!(
        service.routes[0].host,
        Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority { ref value })
            if value == "api.example.test"
    ));
    assert!(matches!(
        service.routes[0].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/v1"
    ));
    assert!(matches!(
        service.routes[0].action,
        HttpRouteAction::Proxy { ref upstream_pool, .. } if upstream_pool == "api"
    ));
    assert!(matches!(
        service.routes[1].action,
        HttpRouteAction::Proxy { ref upstream_pool, .. } if upstream_pool == "fallback"
    ));
    let route = candidate
        .provenance
        .iter()
        .find(|provenance| provenance.path == "/http_services/0/routes/0")
        .expect("conjunction route provenance");
    assert!(route.origins.len() >= 3);
}

#[test]
fn non_equivalent_acl_conjunctions_fail_closed_without_fallback_routes() {
    let duplicate_host = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 30s
frontend public
  bind 127.0.0.1:18080
  acl first hdr(host) -i api.example.test
  acl second hdr(host) -i admin.example.test
  use_backend app if first second
  default_backend fallback
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
backend fallback
  balance roundrobin
  server fallback1 127.0.0.1:3001
";
    let duplicate = import_fixture("duplicate-acl-conjunction.cfg", duplicate_host);
    assert!(
        duplicate
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(duplicate.value().summary().http_services, 0);
    assert!(diagnostic_contains(
        duplicate.diagnostics(),
        "cannot repeat the same Host or path criterion"
    ));

    let case_insensitive_path = String::from_utf8(ACL_CONJUNCTION.to_vec())
        .expect("ACL fixture UTF-8")
        .replace("path_beg /v1", "path_beg -i /v1");
    let blocked = import_fixture(
        "case-insensitive-acl-conjunction.cfg",
        case_insensitive_path.as_bytes(),
    );
    assert!(
        blocked
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(diagnostic_contains(
        blocked.diagnostics(),
        "case-insensitive HAProxy path prefix matching is not canonical"
    ));
}

#[test]
fn conditional_backend_without_default_appends_a_last_catch_all_503() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:8080
  acl app_host hdr(host) app.example
  use_backend app if app_host
backend app
  balance roundrobin
  server app1 127.0.0.1:3001
";
    let lowered = import_fixture("no-default-backend.cfg", source);
    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("503 fallback config");
    let routes = &config.http_services[0].routes;

    assert_eq!(routes.len(), 2);
    assert!(matches!(routes[0].action, HttpRouteAction::Proxy { .. }));
    assert!(routes[1].host.is_none());
    assert!(matches!(
        routes[1].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/"
    ));
    assert!(matches!(
        routes[1].action,
        HttpRouteAction::FixedResponse { status: 503, .. }
    ));
}

#[test]
fn forwardfor_loopback_exception_lowers_to_canonical_source_cidr_policy() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 30s
frontend app
  bind 127.0.0.1:8080 maxconn 10
  maxconn 10
  option forwardfor except 127.0.0.0/8
  default_backend workers
backend workers
  balance roundrobin
  server app1 127.0.0.1:3000
";

    let imported = import_fixture("forwardfor-except.cfg", source);
    let config = imported
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("forwardfor config");
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route")
    };
    assert!(policy.request_headers.iter().any(|mutation| {
        matches!(
            mutation,
            oxiroute_config::HttpRequestHeaderMutation::Set {
                name,
                value: oxiroute_config::HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 8_192,
                    except_source_cidrs,
                },
            } if name == "x-forwarded-for" && except_source_cidrs == &["127.0.0.0/8"]
        )
    }));
}

#[test]
fn raw_routing_subset_retains_the_explicit_unbounded_body_policy() {
    let lowered = import_fixture("http-body-policy.cfg", routing_fixture().as_bytes());

    let config = lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("raw routing config");
    assert_eq!(config.http_services[0].max_request_body_bytes, None);
    assert_has_provenance(lowered.value(), "/http_services/0/max_request_body_bytes");
}

#[test]
fn strict_default_route_http_subset_uses_an_explicit_unbounded_body_policy() {
    let source = b"defaults web
  mode http
  retries 0
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
    let lowered = import_fixture("strict-http.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let candidate = lowered.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("finalized strict HTTP");
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.http_services[0].max_request_body_bytes, None);
    assert_eq!(config.http_services[0].routes.len(), 1);
    assert!(matches!(
        config.http_services[0].routes[0].path,
        HttpPathSelector::RawPrefix { ref value } if value == "/"
    ));
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert_eq!(policy.retry.max_retries, 0);
    assert_has_provenance(candidate, "/http_services/0/max_request_body_bytes");
}

#[test]
fn unconditional_fixed_response_and_redirect_actions_finalize() {
    let fixed = b"frontend health
  mode http
  bind 127.0.0.1:18081
  maxconn 100
  http-request return status 200 content-type text/plain string healthy
";
    let redirect = b"frontend redirect
  mode http
  bind 127.0.0.1:18082
  maxconn 100
  http-request redirect location https://example.test/new code 308
";

    let fixed = import_fixture("fixed-response.cfg", fixed);
    assert!(fixed.diagnostics().is_empty(), "{:?}", fixed.diagnostics());
    assert!(matches!(
        fixed.value().validated().map(oxiroute_config::ValidatedConfig::as_draft).expect("fixed config").http_services[0].routes[0].action,
        HttpRouteAction::FixedResponse { status: 200, ref body, .. } if body == "healthy"
    ));
    assert_has_provenance(fixed.value(), "/http_services/0/routes/0/action/status");
    assert!(
        fixed
            .value()
            .provenance
            .iter()
            .all(|provenance| provenance.path != "/http_services/0/routes/0/action/upstream_pool")
    );

    let redirect = import_fixture("redirect.cfg", redirect);
    assert!(
        redirect.diagnostics().is_empty(),
        "{:?}",
        redirect.diagnostics()
    );
    assert!(matches!(
        redirect
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .expect("redirect config")
            .http_services[0]
            .routes[0]
            .action,
        HttpRouteAction::Redirect { status: 308, .. }
    ));
}

#[test]
fn representable_forward_header_mutations_lower_into_proxy_policy() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 30s
  timeout server 30s
frontend public
  bind 127.0.0.1:18083
  maxconn 100
  http-request set-header X-Client-IP %[src]
  http-request del-header X-Remove
  default_backend app
backend app
  balance roundrobin
  http-response set-header X-Frame-Options same-origin
  http-response del-header X-Powered-By
  server app1 127.0.0.1:3000
";
    let lowered = import_fixture("header-mutations.cfg", source);

    assert!(
        lowered.diagnostics().is_empty(),
        "{:?}",
        lowered.diagnostics()
    );
    let route = &lowered
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("header config")
        .http_services[0]
        .routes[0];
    let HttpRouteAction::Proxy { policy, .. } = &route.action else {
        panic!("proxy action");
    };
    assert_eq!(policy.request_headers.len(), 2);
    assert_eq!(policy.response_headers.len(), 2);
    assert_has_provenance(
        lowered.value(),
        "/http_services/0/routes/0/action/policy/request_headers",
    );
}

fn routing_fixture() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:8080\n  maxconn 100\n  acl api_path path_beg /api\n  use_backend api if api_path\n  default_backend fallback\nbackend api\n  balance roundrobin\n  server api1 127.0.0.1:3001\nbackend fallback\n  balance roundrobin\n  server fallback1 127.0.0.1:3003\n"
        .into()
}
