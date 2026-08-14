#![cfg(unix)]

use std::path::PathBuf;

use oxiroute_import::{
    ImportReportEnvelope, SourceFile, SourceId,
    haproxy::{LoadedSource, import_sources},
};

const SOURCE_PATH: &str = "fixtures/report-v1-blocked.cfg";
const SOURCE: &[u8] = b"defaults web\n  mode http\n  timeout connect 5s\n  timeout client 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18081\n  use_backend app if { path /healthz }\n  default_backend app\nbackend app\n  server app1 127.0.0.1:3000\n";

#[test]
fn blocked_haproxy_report_schema_v1_is_exact() {
    let path = PathBuf::from(SOURCE_PATH);
    let report = import_sources(&[LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        path: path.clone(),
        source: SourceFile::from_path(SourceId::new(0), path.clone(), SOURCE),
    }]);
    let json = ImportReportEnvelope::from_haproxy(&report, &[path])
        .to_json()
        .expect("report JSON");

    assert_eq!(
        json,
        include_str!("fixtures/report-v1-blocked.json").trim_end()
    );
}
