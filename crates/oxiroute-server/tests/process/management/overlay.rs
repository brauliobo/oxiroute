use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

use oxiroute_config_source::ConfigFormat;
use serde_json::{Value, json};

use super::config_source::Daemon;
use crate::http_support::http_request;
use crate::process_support::{ServerProcess, reserve_tcp_address};
use crate::revision_support::active_revision;
use crate::shared::{TOKEN, management_config, public_pool, recording_candidate};

const BANNER: &str = "// Edited by hand: this comment must survive every save.\n";

fn overlay_path(daemon: &Daemon) -> PathBuf {
    let mut path = daemon.server.config_path.clone().into_os_string();
    path.push(".overlay.json");
    PathBuf::from(path)
}

async fn change(daemon: &Daemon, method: &str, path: &str, mut body: Value) -> Value {
    body["expectedActiveRevision"] =
        json!(active_revision(daemon.address, &daemon.authorization).await);
    body["targets"] = json!([{ "pool": "public", "server": "origin-a" }]);
    let response = daemon.send(method, path, None, &body).await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.json()
}

async fn overlay(daemon: &Daemon) -> Value {
    let response = daemon.get("/api/v1/overlay").await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.json()
}

async fn origin(daemon: &Daemon) -> Value {
    let response = daemon.get("/api/v1/servers").await;
    response.json()["servers"][0]["server"].clone()
}

async fn restart(daemon: &mut Daemon) {
    daemon.server.restart();
    daemon.server.wait_for_tcp(daemon.address).await;
}

fn settings(overlay: &Value) -> Vec<(String, String, String)> {
    overlay["settings"]
        .as_array()
        .expect("settings")
        .iter()
        .map(|setting| {
            (
                setting["setting"].as_str().expect("setting").to_owned(),
                setting["value"].as_str().expect("value").to_owned(),
                setting["status"].as_str().expect("status").to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn a_persisted_drain_survives_a_restart_and_a_plain_one_does_not() {
    let mut daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    assert_eq!(overlay(&daemon).await["state"], "empty");

    let plain = change(
        &daemon,
        "POST",
        "/api/v1/servers/administrative-state",
        json!({ "state": "drain" }),
    )
    .await;
    assert_eq!(plain["persistence"], "runtime");
    assert!(!overlay_path(&daemon).exists());
    restart(&mut daemon).await;
    assert_eq!(origin(&daemon).await["administrativeState"], "ready");

    let kept = change(
        &daemon,
        "POST",
        "/api/v1/servers/administrative-state",
        json!({ "state": "drain", "persist": true }),
    )
    .await;
    assert_eq!(kept["persistence"], "overlay");
    let file = overlay_path(&daemon);
    assert_eq!(
        fs::metadata(&file).expect("overlay").permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(daemon.file(), daemon.text);

    restart(&mut daemon).await;

    assert_eq!(origin(&daemon).await["administrativeState"], "drain");
    let shown = overlay(&daemon).await;
    assert_eq!(shown["state"], "applied");
    assert_eq!(
        settings(&shown),
        [("state".into(), "drain".into(), "applied".into())]
    );
    assert_eq!(shown["settings"][0]["target"], "public/origin-a");
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn every_persisted_setting_is_applied_again_after_a_restart() {
    let mut daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/servers/health-override",
            json!({ "health": "down" }),
        ),
        (
            "POST",
            "/api/v1/servers/checks",
            json!({ "enabled": false }),
        ),
        (
            "PUT",
            "/api/v1/servers/max-connections",
            json!({ "maxConnections": 7 }),
        ),
        ("PUT", "/api/v1/servers/weight", json!({ "weight": 5 })),
    ] {
        let mut body = body;
        body["persist"] = json!(true);
        assert_eq!(
            change(&daemon, method, path, body).await["persistence"],
            "overlay"
        );
    }

    restart(&mut daemon).await;

    let server = origin(&daemon).await;
    assert_eq!(server["healthOverride"], "down");
    assert_eq!(server["checksEnabled"], false);
    assert_eq!(server["maxConnections"], 7);
    assert_eq!(server["weight"], 5);
    assert_eq!(settings(&overlay(&daemon).await).len(), 4);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_later_change_shows_as_diverged_and_a_persisted_reset_removes_the_entry() {
    let mut daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let drain = json!({ "state": "drain", "persist": true });
    change(
        &daemon,
        "POST",
        "/api/v1/servers/administrative-state",
        drain,
    )
    .await;
    change(
        &daemon,
        "POST",
        "/api/v1/servers/administrative-state",
        json!({ "state": "ready" }),
    )
    .await;

    let shown = overlay(&daemon).await;
    assert_eq!(shown["state"], "diverged");
    assert_eq!(
        settings(&shown),
        [("state".into(), "drain".into(), "diverged".into())]
    );
    restart(&mut daemon).await;
    assert_eq!(origin(&daemon).await["administrativeState"], "drain");

    let reset = json!({ "state": "ready", "persist": true });
    change(
        &daemon,
        "POST",
        "/api/v1/servers/administrative-state",
        reset,
    )
    .await;

    assert!(!overlay_path(&daemon).exists());
    assert_eq!(overlay(&daemon).await["state"], "empty");
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_setting_of_a_removed_server_is_reported_as_an_orphan() {
    let mut daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    fs::write(
        overlay_path(&daemon),
        r#"{"schemaVersion": 1, "servers": [{"pool": "public", "server": "gone", "weight": 4}]}"#,
    )
    .expect("overlay");
    restart(&mut daemon).await;

    let shown = overlay(&daemon).await;

    assert_eq!(shown["state"], "diverged");
    assert_eq!(
        settings(&shown),
        [("weight".into(), "4".into(), "orphan".into())]
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn an_unreadable_overlay_leaves_the_configuration_running_and_clear_repairs_it() {
    let mut daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    fs::write(overlay_path(&daemon), "{ not an overlay").expect("overlay");
    restart(&mut daemon).await;

    assert_eq!(origin(&daemon).await["administrativeState"], "ready");
    assert_eq!(overlay(&daemon).await["state"], "unreadable");
    let body = json!({
        "state": "drain",
        "persist": true,
        "targets": [{ "pool": "public", "server": "origin-a" }],
        "expectedActiveRevision": active_revision(daemon.address, &daemon.authorization).await,
    });
    let refused = daemon
        .send("POST", "/api/v1/servers/administrative-state", None, &body)
        .await;
    assert_eq!(refused.status, 500, "{}", refused.text());
    assert_eq!(origin(&daemon).await["administrativeState"], "ready");

    let cleared = daemon
        .send("POST", "/api/v1/overlay/clear", None, &json!({}))
        .await;

    assert_eq!(cleared.status, 200, "{}", cleared.text());
    assert!(!overlay_path(&daemon).exists());
    assert_eq!(overlay(&daemon).await["state"], "empty");
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn clearing_removes_the_file_and_leaves_the_running_state() {
    let mut daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let weight = json!({ "weight": 5, "persist": true });
    change(&daemon, "PUT", "/api/v1/servers/weight", weight).await;

    let cleared = daemon
        .send("POST", "/api/v1/overlay/clear", None, &json!({}))
        .await;

    assert_eq!(cleared.status, 200, "{}", cleared.text());
    assert_eq!(cleared.json()["changed"], 1);
    assert_eq!(origin(&daemon).await["weight"], 5);
    restart(&mut daemon).await;
    assert_eq!(origin(&daemon).await["weight"], 1);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn promoting_writes_the_limits_into_the_configuration_in_its_own_format() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    change(
        &daemon,
        "PUT",
        "/api/v1/servers/max-connections",
        json!({ "maxConnections": 7, "persist": true }),
    )
    .await;
    change(
        &daemon,
        "PUT",
        "/api/v1/servers/weight",
        json!({ "weight": 5, "persist": true }),
    )
    .await;
    let revision = daemon.disk_revision().await;

    let promoted = daemon
        .send(
            "POST",
            "/api/v1/overlay/promote",
            Some(&revision),
            &json!({}),
        )
        .await;

    assert_eq!(promoted.status, 200, "{}", promoted.text());
    assert_eq!(promoted.json()["written"], true);
    assert!(
        daemon.file().contains("max_connections 7"),
        "{}",
        daemon.file()
    );
    let shown = overlay(&daemon).await;
    assert_eq!(
        settings(&shown),
        [("weight".into(), "5".into(), "applied".into())]
    );
    let history = daemon.get("/api/v1/config/history").await.json();
    assert_eq!(history["entries"].as_array().map(Vec::len), Some(1));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn promoting_needs_the_current_revision_and_something_to_promote() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let revision = daemon.disk_revision().await;
    let nothing = daemon
        .send(
            "POST",
            "/api/v1/overlay/promote",
            Some(&revision),
            &json!({}),
        )
        .await;
    assert_eq!(nothing.status, 200, "{}", nothing.text());
    assert_eq!(nothing.json()["outcome"], "nothing_to_promote");

    change(
        &daemon,
        "PUT",
        "/api/v1/servers/max-connections",
        json!({ "maxConnections": 7, "persist": true }),
    )
    .await;
    let stale = daemon
        .send(
            "POST",
            "/api/v1/overlay/promote",
            Some(&"0".repeat(64)),
            &json!({}),
        )
        .await;

    assert_eq!(stale.status, 409, "{}", stale.text());
    assert_eq!(daemon.file(), daemon.text);
    assert_eq!(settings(&overlay(&daemon).await).len(), 1);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn the_overlay_needs_the_management_token() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    for (method, path) in [
        ("GET", "/api/v1/overlay"),
        ("POST", "/api/v1/overlay/clear"),
        ("POST", "/api/v1/overlay/promote"),
    ] {
        let response =
            crate::http_support::http_request(daemon.address, method, path, &[], &[]).await;
        assert_eq!(response.status, 401, "{method} {path}: {}", response.text());
    }
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_persisted_listener_drain_is_applied_again_after_a_restart() {
    let root = tempfile::tempdir().expect("recording root");
    let address = reserve_tcp_address();
    let mut config = management_config(address, None);
    config.upstream_pools = vec![public_pool()];
    let config = recording_candidate(&config, root.path());
    let mut daemon =
        Daemon::start_with("oxiroute.kdl", ConfigFormat::Kdl, BANNER, config, address).await;
    let body = json!({
        "listeners": ["recording-wire"],
        "state": "drain",
        "persist": true,
        "expectedActiveRevision": active_revision(daemon.address, &daemon.authorization).await,
    });
    let kept = daemon
        .send(
            "POST",
            "/api/v1/listeners/administrative-state",
            None,
            &body,
        )
        .await;
    assert_eq!(kept.status, 200, "{}", kept.text());
    assert_eq!(kept.json()["persistence"], "overlay");

    restart(&mut daemon).await;

    let listeners = daemon.get("/api/v1/listeners").await.json();
    let wire = listeners["listeners"]
        .as_array()
        .expect("listeners")
        .iter()
        .find(|listener| listener["name"] == "recording-wire")
        .expect("listener")
        .clone();
    assert_eq!(wire["administrativeState"], "drain");
    let shown = overlay(&daemon).await;
    assert_eq!(shown["settings"][0]["target"], "recording-wire");
    assert_eq!(shown["settings"][0]["status"], "applied");
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_persisted_drain_survives_a_restart_of_a_supervised_daemon() {
    let address = reserve_tcp_address();
    let mut config = management_config(address, None);
    config.upstream_pools = vec![public_pool()];
    let validated = config.validate().expect("valid process config");
    let mut server = ServerProcess::start_supervised(&validated, TOKEN);
    server.wait_for_tcp(address).await;
    let authorization = format!("Bearer {TOKEN}");
    let headers = [
        ("Authorization", authorization.as_str()),
        ("Content-Type", "application/json"),
    ];
    let body = json!({
        "targets": [{ "pool": "public", "server": "origin-a" }],
        "state": "drain",
        "persist": true,
        "expectedActiveRevision": active_revision(address, &authorization).await,
    });
    let kept = http_request(
        address,
        "POST",
        "/api/v1/servers/administrative-state",
        &headers,
        body.to_string().as_bytes(),
    )
    .await;
    assert_eq!(kept.status, 200, "{}", kept.text());
    assert_eq!(kept.json()["persistence"], "overlay");

    server.restart();
    server.wait_for_tcp(address).await;

    let servers = http_request(
        address,
        "GET",
        "/api/v1/servers",
        &[("Authorization", authorization.as_str())],
        &[],
    )
    .await
    .json();
    assert_eq!(
        servers["servers"][0]["server"]["administrativeState"],
        "drain"
    );
    server.shutdown();
}
