use oxiroute_import::{DiagnosticStage, E_INVALID_VALUE};

use super::common::import_fixture;

#[test]
fn canonical_validation_failure_is_a_blocking_validate_diagnostic() {
    let source = b"defaults tcp_defaults
  mode tcp
  retries 0
  timeout connect 10s
  timeout client 5m
  timeout server 5m
frontend first
  bind 127.0.0.1:15432
  maxconn 100
  default_backend pool
frontend second
  bind 127.0.0.1:15432
  maxconn 100
  default_backend pool
backend pool
  balance roundrobin
  server primary 127.0.0.1:5432
";
    let lowered = import_fixture("overlapping-binds.cfg", source);

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(lowered.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_INVALID_VALUE
            && diagnostic.stage() == DiagnosticStage::Validate
            && diagnostic.message().contains("overlap")
    }));
}

#[test]
fn public_source_import_carries_syntax_diagnostics_through_finalization() {
    let lowered = import_fixture("syntax.cfg", b"frontend public\n  bind 127.0.0.1:8080");

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(
        lowered
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == oxiroute_import::haproxy::E_SYNTAX)
    );
}
