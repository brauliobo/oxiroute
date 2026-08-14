use std::path::Path;

use oxiroute_config_source::resolve_source;

#[test]
fn native_haproxy_reference_preserves_http_check_send_policy() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/haproxy/http-check-send.cfg");
    let path = serde_json::to_string(fixture.to_str().unwrap()).unwrap();
    let source = format!("haproxy_server = {{ paths = [{path}] }}");

    let resolved = resolve_source(Path::new("health-check.hocon"), source.as_bytes())
        .expect("representable HAProxy health-check root");
    let health = resolved.config.as_draft().upstream_pools[0]
        .health_check
        .as_ref()
        .expect("native health check");

    assert_eq!(health.path.as_deref(), Some("/healthz"));
    assert_eq!(health.host.as_deref(), Some("backend.internal"));
    assert_eq!(health.expected_status, Some(204));
    assert!(resolved.native_references[0].evidence.candidate.finalized);
    assert!(
        resolved.native_references[0]
            .evidence
            .candidate
            .provenance
            .iter()
            .any(|entry| entry.path == "/upstream_pools/0/health_check/path")
    );
}
