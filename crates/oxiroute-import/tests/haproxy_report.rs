#![cfg(unix)]

use std::path::Path;

use oxiroute_import::{ImportReportEnvelope, haproxy::import_roots};
use serde_json::Value;

#[test]
fn http_check_send_report_retains_health_provenance_and_source_table() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/haproxy/http-check-send.cfg");
    let report = ImportReportEnvelope::from_haproxy(
        &import_roots(std::slice::from_ref(&path)),
        std::slice::from_ref(&path),
    );
    let value: Value = serde_json::from_str(&report.to_json().expect("HAProxy report JSON"))
        .expect("HAProxy report object");

    assert_eq!(value["candidate"]["finalized"], true);
    assert_eq!(
        value["sourceGraph"]["sources"]
            .as_array()
            .expect("source table")
            .len(),
        1
    );
    assert!(
        value["candidate"]["provenance"]
            .as_array()
            .expect("candidate provenance")
            .iter()
            .any(|entry| entry["path"] == "/upstream_pools/0/health_check/path")
    );
    assert!(
        value["candidate"]["provenance"]
            .as_array()
            .expect("candidate provenance")
            .iter()
            .any(|entry| entry["path"] == "/upstream_pools/0/health_check/expected_status")
    );
}
