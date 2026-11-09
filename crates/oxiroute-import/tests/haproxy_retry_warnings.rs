use std::path::PathBuf;

use oxiroute_import::{
    Report, Severity, SourceFile, SourceId,
    haproxy::{CanonicalCandidate, LoadedSource, import_sources},
};

const SOURCE: &str = "defaults web
  mode http
  retries 2
  option redispatch
  retry-on 500
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

fn import(source: &str) -> Report<CanonicalCandidate> {
    let path = PathBuf::from("retry-on.cfg");
    import_sources(&[LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        source: SourceFile::from_path(SourceId::new(0), path.clone(), source.as_bytes()),
        path,
    }])
}

fn codes(report: &Report<CanonicalCandidate>) -> Vec<&'static str> {
    report
        .diagnostics()
        .iter()
        .inspect(|diagnostic| assert_eq!(diagnostic.severity(), Severity::Warning))
        .map(|diagnostic| diagnostic.code().as_str())
        .collect()
}

#[test]
fn status_retries_without_passive_health_are_reported_and_still_import() {
    let report = import(SOURCE);

    assert_eq!(codes(&report), ["W_PASSIVE_HEALTH_MISSING"]);
    assert!(report.value().validated().is_some());
}

#[test]
fn connect_only_retries_import_without_warnings() {
    let report = import(&SOURCE.replace("retry-on 500", "retry-on conn-failure"));

    assert!(report.diagnostics().is_empty());
}
