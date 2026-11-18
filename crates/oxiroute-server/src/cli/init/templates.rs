use std::path::Path;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use serde_json::{Value, json};

use super::args::Kind;

pub(super) const TOKEN_FILE: &str = "management.token";
pub(super) const PUBLIC_DIRECTORY: &str = "public";

fn skeleton(base: &Path) -> Value {
    json!({
        "version": 1,
        "management": { "bind": "127.0.0.1:9900", "token_file": base.join(TOKEN_FILE) },
        "certificates": [], "tls_profiles": [], "listeners": [], "cache_stores": [],
        "upstream_pools": [], "http_services": [], "forward_proxy_services": [],
        "rtmp_services": [], "l4_services": [],
    })
}

fn listener(name: &str, protocol: &str, address: &str) -> Value {
    json!({
        "name": name, "protocol": protocol, "service": name,
        "bind": { "type": "socket", "address": address },
    })
}

fn http_service(action: &Value) -> Value {
    json!({
        "name": "web",
        "routes": [{ "path": { "kind": "segment_prefix", "value": "/" }, "action": action }],
    })
}

fn parts(kind: Kind, base: &Path) -> Value {
    match kind {
        Kind::Proxy => json!({
            "listeners": [listener("web", "http", "0.0.0.0:8080")],
            "upstream_pools": [{
                "name": "app",
                "servers": [{
                    "name": "app-1",
                    "endpoint": { "type": "socket", "address": "127.0.0.1:3000" },
                }],
            }],
            "http_services": [http_service(&json!({
                "type": "proxy", "upstream_pool": "app", "policy": {},
            }))],
        }),
        Kind::Static => json!({
            "listeners": [listener("web", "http", "0.0.0.0:8080")],
            "http_services": [http_service(&json!({
                "type": "static_files", "root_directory": base.join(PUBLIC_DIRECTORY),
            }))],
        }),
        Kind::Rtmp => json!({
            "listeners": [listener("live", "rtmp", "0.0.0.0:1935")],
            "rtmp_services": [{
                "name": "live",
                "applications": [{
                    "name": "live", "live": true, "idle_streams": true, "recorders": [],
                }],
            }],
        }),
    }
}

pub(super) fn config(kind: Kind, base: &Path) -> Result<ValidatedConfig, String> {
    let mut document = skeleton(base);
    let object = document.as_object_mut().expect("the skeleton is an object");
    object.extend(
        parts(kind, base)
            .as_object()
            .cloned()
            .expect("the parts are an object"),
    );
    serde_json::from_value::<ConfigDraft>(document)
        .map_err(|error| error.to_string())?
        .validate()
        .map_err(|error| error.to_string())
}
