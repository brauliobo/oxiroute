use std::path::Path;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::json;

const ONE_MIB: usize = 1024 * 1024;

/// A router-sized import repeats full route policies and MIME tables; its canonical rendering passes 1 MiB.
fn many_route_config(routes: usize) -> ValidatedConfig {
    let routes = (0..routes)
        .map(|index| {
            json!({
                "path": {"kind": "exact", "value": format!("/route-{index}-{}", "x".repeat(2_000))},
                "action": {"type": "fixed_response", "status": 204},
            })
        })
        .collect::<Vec<_>>();
    serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "upstream_pools": [{"name": "app", "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]}],
        "http_services": [{"name": "web", "routes": routes}],
        "listeners": [{"name": "edge", "bind": {"type": "socket", "address": "127.0.0.1:8080"},
                       "protocol": "http", "service": "web"}],
    }))
    .expect("authored configuration")
    .validate()
    .expect("valid configuration")
}

#[test]
fn a_rendering_above_one_mib_round_trips() {
    let config = many_route_config(700);
    let rendered = render_config(ConfigFormat::Kdl, &config).expect("render");
    assert!(rendered.len() > ONE_MIB, "{} bytes", rendered.len());
    let resolved =
        resolve_source_with_format(Path::new("config"), rendered.as_bytes(), ConfigFormat::Kdl)
            .expect("resolve");
    assert_eq!(resolved.config, config);
}
