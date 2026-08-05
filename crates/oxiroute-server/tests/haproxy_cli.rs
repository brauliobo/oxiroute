use std::{path::Path, process::Command};

use serde_json::Value;

#[test]
fn http_check_send_cli_report_is_deterministic_and_finalized() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/http-check-send.cfg");
    let first = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .args(["import", "haproxy", path.to_str().unwrap()])
        .output()
        .expect("HAProxy health-check report");
    let second = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .args(["import", "haproxy", path.to_str().unwrap()])
        .output()
        .expect("repeated HAProxy health-check report");

    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    let report: Value = serde_json::from_slice(&first.stdout).expect("HAProxy report JSON");
    assert_eq!(report["candidate"]["finalized"], true);
    assert!(
        report["candidate"]["provenance"]
            .as_array()
            .expect("candidate provenance")
            .iter()
            .any(|entry| entry["path"] == "/upstream_pools/0/health_check/path")
    );
}
