use std::{fs, process::Command};

use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::json;
use tempfile::TempDir;

fn check(pool_extra: &serde_json::Value, retry: &serde_json::Value) -> String {
    let mut pool = json!({
        "name": "origin",
        "endpoints": [{"type": "socket", "address": "127.0.0.1:3000"}]
    });
    pool.as_object_mut()
        .unwrap()
        .extend(pool_extra.as_object().unwrap().clone());
    let config: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": [pool],
        "http_services": [{"name": "web", "routes": [{
            "path": {"kind": "exact", "value": "/"},
            "methods": ["POST"],
            "action": {
                "type": "proxy",
                "upstream_pool": "origin",
                "policy": {"retry": retry}
            }
        }]}]
    }))
    .expect("authored configuration");
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("oxiroute.kdl");
    let rendered = render_config(ConfigFormat::Kdl, &config.validate().expect("valid config"));
    fs::write(&path, rendered.expect("render")).expect("source");

    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .args(["config", "check", path.to_str().unwrap()])
        .output()
        .expect("config check");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn config_check_reports_retry_warnings_without_failing() {
    let retry = json!({"max_retries": 1, "triggers": [], "response_statuses": [500]});
    let stdout = check(&json!({}), &retry);

    assert!(stdout.starts_with("configuration is valid and runtime-preparable\n"));
    assert!(
        stdout.contains("warning: W_RETRY_NEVER_REPLAYS"),
        "{stdout}"
    );
    assert!(
        stdout.contains("warning: W_PASSIVE_HEALTH_MISSING"),
        "{stdout}"
    );
}

#[test]
fn config_check_without_warnings_prints_only_the_verdict() {
    let stdout = check(&json!({}), &json!({"max_retries": 1}));

    assert_eq!(stdout, "configuration is valid and runtime-preparable\n");
}
