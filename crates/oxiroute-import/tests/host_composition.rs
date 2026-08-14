use std::path::Path;

use oxiroute_config::compose_validated_configs;
use oxiroute_import::{
    haproxy::{PreprocessingEnvironment, import_roots_with_environment},
    nginx::{
        NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay, NginxHostTimezoneOverlay,
        NginxImportOptions, NginxRecordingRootOverlay, import_root_with_options,
    },
};

#[test]
fn phoenix_nginx_and_haproxy_candidates_compose_as_one_canonical_host() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live/phoenix");
    let nginx = import_root_with_options(
        Path::new("nginx.conf"),
        &fixture.join("nginx"),
        &NginxImportOptions {
            host_timezones: vec![NginxHostTimezoneOverlay {
                timezone: "America/Bahia".into(),
            }],
            default_access_log: Some(NginxDefaultAccessLogOverlay {
                path: "/var/lib/oxiroute/http-access.jsonl".into(),
            }),
            recording_root: Some(NginxRecordingRootOverlay {
                path: "/mnt/cloud/4tb/cam-rtmp".into(),
            }),
            default_error_page: Some(NginxDefaultErrorPageOverlay {
                server: "nginx/1.30.2".into(),
            }),
            ..NginxImportOptions::default()
        },
    );
    let haproxy = import_roots_with_environment(
        &[fixture.join("haproxy.cfg")],
        PreprocessingEnvironment {
            node_ip: "10.0.0.11".parse().unwrap(),
            gpu1_defined: false,
        },
    );
    let nginx = nginx
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::to_draft)
        .expect("finalized nginx candidate");
    let haproxy = haproxy
        .value()
        .validated()
        .map(oxiroute_config::ValidatedConfig::to_draft)
        .expect("finalized HAProxy candidate");
    let listener_count = nginx.listeners.len() + haproxy.listeners.len();

    let composed = compose_validated_configs(vec![nginx, haproxy]).expect("composed Phoenix host");
    let composed = composed.as_draft();

    assert_eq!(composed.listeners.len(), listener_count);
    assert!(!composed.http_services.is_empty());
    assert!(!composed.rtmp_services.is_empty());
    assert!(!composed.upstream_pools.is_empty());
}
