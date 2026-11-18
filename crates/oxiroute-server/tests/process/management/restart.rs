use oxiroute_config::{RegistryEndpoint, RegistryRole};
use serde_json::{Value, json};

use crate::daemon::Daemon;
use crate::http_support::http_request;

fn required(generation: &Value) -> bool {
    generation["restartRequired"]["required"] == true
}

async fn one_reason_on_every_surface(supervised: bool) {
    let daemon = Daemon::start(supervised).await;
    let before = daemon.generation().await;
    assert_eq!(before["restartRequired"]["required"], false);
    assert_eq!(before["restartRequired"]["reasons"], json!([]));
    let active = before["activeRevision"]
        .as_str()
        .expect("active")
        .to_owned();

    let candidate = daemon.candidate(true, 7);
    daemon.write(&candidate);
    daemon.hang_up();
    let generation = daemon
        .wait_generation("the rest of the candidate to apply", |generation| {
            required(generation) && generation["activeRevision"] != active
        })
        .await;
    let applied = generation["activeRevision"].clone();
    assert_eq!(daemon.max_connections().await, 7);
    let restart = generation["restartRequired"].clone();
    let reason = restart["reasons"][0].clone();
    assert_eq!(
        restart["reasons"].as_array().map(Vec::len),
        Some(1),
        "{restart}"
    );
    assert_eq!(reason["code"], "listener_ipv6_only");
    assert_eq!(reason["path"], "/config/listeners/live/bind/ipv6_only");
    assert!(
        reason["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("ipv6_only false"))
    );
    assert!(restart["since"].as_u64().is_some_and(|since| since > 0));
    let config = daemon.get("/api/v1/config").await;
    assert_eq!(restart["candidateRevision"], config["candidateRevision"]);
    assert_ne!(restart["candidateRevision"], applied);

    assert_eq!(generation["degraded"], false, "{generation}");
    assert_eq!(generation["quarantinedRevision"], Value::Null);
    assert_eq!(generation["lastFailure"], Value::Null);
    assert_eq!(generation["lastRejection"], Value::Null);
    assert_eq!(
        daemon.get("/api/v1/status").await["restartRequired"],
        restart
    );
    assert!(daemon.accepts_ipv4().await);
    assert_eq!(daemon.restart_gauge().await, "oxiroute_restart_required 1");

    let reload = daemon
        .request(
            "POST",
            "/api/v1/generations/reload",
            &json!({"expectedActiveRevision": applied}),
        )
        .await;
    assert_eq!(reload.status, 202, "{}", reload.text());
    let reload = reload.json();
    assert_eq!(reload["outcome"], "startup_requested");
    assert_eq!(reload["candidateRevision"], applied);
    assert_eq!(reload["restartRequired"]["reasons"], restart["reasons"]);

    let body = json!({ "config": candidate });
    let validated = daemon
        .request("POST", "/api/v1/config/validate", &body)
        .await;
    assert_eq!(validated.status, 200, "{}", validated.text());
    let validated = validated.json();
    assert_eq!(validated["restartRequired"], true);
    assert_eq!(validated["restartReasons"], restart["reasons"]);
    let diagnostic = validated["diagnostics"]
        .as_array()
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|d| d["code"] == "I_RESTART_REQUIRED")
        })
        .expect("restart diagnostic");
    assert_eq!(diagnostic["path"], reason["path"]);
    assert_eq!(diagnostic["message"], reason["summary"]);

    let after = daemon.generation().await;
    assert_eq!(after["degraded"], false, "{after}");
    assert_eq!(after["quarantinedRevision"], Value::Null);
    assert_eq!(after["failures"], 0);
    assert_eq!(after["restartRequired"], restart);
    let ready = http_request(daemon.management, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200, "{}", ready.text());

    daemon.write(&daemon.candidate(false, 7));
    daemon.hang_up();
    let cleared = daemon
        .wait_generation("the restart no longer required", |g| !required(g))
        .await;
    assert_eq!(cleared["restartRequired"]["reasons"], json!([]));
    assert_eq!(cleared["restartRequired"]["candidateRevision"], Value::Null);
    assert_eq!(cleared["activeRevision"], applied);
    assert_eq!(daemon.restart_gauge().await, "oxiroute_restart_required 0");

    let log = daemon.server.shutdown_output();
    assert!(log.contains("only a restart applies"), "{log}");
}

#[tokio::test]
async fn direct_restart_reasons_are_the_same_and_the_rest_applies() {
    one_reason_on_every_surface(false).await;
}

#[tokio::test]
async fn supervised_restart_reasons_are_the_same_and_the_rest_applies() {
    one_reason_on_every_surface(true).await;
}

#[tokio::test]
async fn the_direct_runtime_keeps_its_registry_settings_and_applies_the_rest() {
    let daemon = Daemon::launch(false, true, &[]).await;
    let socket = daemon.path.join("registry.sock");
    let mut candidate = daemon.candidate(false, 7);
    candidate.registry.as_mut().expect("registry").endpoints = vec![RegistryEndpoint::Unix {
        path: socket.clone(),
        mode: 0o660,
        role: RegistryRole::Read,
        pools: Vec::new(),
    }];
    let active = daemon.generation().await["activeRevision"].clone();
    daemon.write(&candidate);

    let generation = daemon
        .wait_generation("the rest of the candidate to apply", |generation| {
            required(generation) && generation["activeRevision"] != active
        })
        .await;

    let reason = &generation["restartRequired"]["reasons"][0];
    assert_eq!(reason["code"], "registry_settings");
    assert_eq!(reason["path"], "/config/registry");
    assert_eq!(daemon.max_connections().await, 7);
    assert!(
        !socket.exists(),
        "the registry endpoint waits for a restart"
    );
}
