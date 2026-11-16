use oxiroute_import::haproxy::{E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FORM};

use super::common::{code_count, occurrence_count, resolve_bytes};

#[test]
fn use_backend_rules_retain_first_match_source_order() {
    let contents = b"defaults web
  mode http
frontend public
  bind :80
  acl host_first hdr(host) -i first.test
  acl host_second hdr(host) -i second.test
  use_backend first_pool if host_first
  use_backend second_pool if host_second
  default_backend fallback
backend second_pool
  server second second.test:3000
backend fallback
  server fallback fallback.test:3000
backend first_pool
  server first first.test:3000
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    let frontend = &report.value().frontends[0];
    assert_eq!(
        frontend
            .use_backends
            .iter()
            .map(|rule| rule.value.backend.name.as_slice())
            .collect::<Vec<_>>(),
        [b"first_pool".as_slice(), b"second_pool".as_slice()]
    );
    assert_eq!(
        frontend
            .use_backends
            .iter()
            .map(|rule| rule.value.conditions[0].name.as_slice())
            .collect::<Vec<_>>(),
        [b"host_first".as_slice(), b"host_second".as_slice()]
    );
}

#[test]
fn resolves_positive_host_and_path_acl_conjunction_with_each_reference() {
    let contents = b"frontend public
  mode http
  bind :80
  acl app_host hdr(host) -i api.example.test
  acl app_path path_beg /api
  use_backend app if app_host app_path
backend app
  server app1 127.0.0.1:3000
";
    let (configuration, report) = resolve_bytes(contents);

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let rule = &report.value().frontends[0].use_backends[0];
    assert_eq!(rule.value.conditions.len(), 2);
    assert_eq!(
        rule.value
            .conditions
            .iter()
            .map(|condition| condition.name.as_slice())
            .collect::<Vec<_>>(),
        [b"app_host".as_slice(), b"app_path".as_slice()]
    );
    assert_eq!(rule.provenance.references.len(), 3);
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
}

#[test]
fn dynamic_and_negated_acl_conditions_remain_fail_closed() {
    let dynamic = b"frontend public
  mode http
  bind :80
  use_backend app if { path /api }
backend app
  server app1 127.0.0.1:3000
";
    let (_, dynamic_report) = resolve_bytes(dynamic);
    assert!(dynamic_report.has_errors());
    assert_eq!(code_count(&dynamic_report, E_UNSUPPORTED_FORM), 1);
    assert_eq!(code_count(&dynamic_report, E_UNRESOLVED_REFERENCE), 0);
    assert!(dynamic_report.value().frontends[0].use_backends.is_empty());

    let negated = b"frontend public
  mode http
  bind :80
  acl app_host hdr(host) api.example.test
  acl app_path path_beg /api
  use_backend app unless ! app_host app_path
backend app
  server app1 127.0.0.1:3000
";
    let (_, negated_report) = resolve_bytes(negated);
    let rule = &negated_report.value().frontends[0].use_backends[0];
    assert!(negated_report.diagnostics().is_empty());
    assert_eq!(
        rule.value.polarity,
        oxiroute_import::haproxy::ConditionPolarity::Unless
    );
    assert!(rule.value.condition_negated);
}
