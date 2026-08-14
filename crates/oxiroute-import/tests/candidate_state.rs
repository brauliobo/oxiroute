use std::fs;

use oxiroute_import::{Severity, nginx::import_root};
use tempfile::tempdir;

#[test]
fn finalized_candidate_exposes_one_validated_state() {
    let directory = tempdir().expect("candidate state directory");
    let path = directory.path().join("nginx.conf");
    fs::write(
        &path,
        b"events {} http { access_log off; server { listen 127.0.0.1:18080; location / { return 200 ok; } } }",
    )
    .expect("valid nginx source");

    let report = import_root(&path, directory.path());
    let validated = report
        .candidate
        .validated()
        .expect("complete import is validated");

    assert_eq!(
        report.candidate.summary().listeners,
        validated.as_draft().listeners.len()
    );
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    );
    assert!(report.blocked_http_services.is_empty());
    assert!(report.blocked_rtmp_services.is_empty());
    assert!(report.blocked_stream_services.is_empty());
}

#[test]
fn blocked_candidate_withholds_importer_owned_config_capability() {
    let directory = tempdir().expect("candidate state directory");
    let path = directory.path().join("nginx.conf");
    fs::write(
        &path,
        b"events {} http { server { listen 127.0.0.1:18080; proxy_buffering off; return 204; } }",
    )
    .expect("blocked nginx source");

    let report = import_root(&path, directory.path());

    assert!(report.candidate.validated().is_none());
    assert_eq!(report.candidate.summary().version, 1);
    let debug = format!("{:?}", report.candidate);
    assert!(!debug.contains("Config {"));
    assert!(debug.contains("summary"));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    );
}
