use std::{fs, os::unix::fs::PermissionsExt as _, time::Duration};

use oxiroute_config::{ListenerBind, Management};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::http_support::http_request;
use crate::process_support::{ServerProcess, reserve_tcp_address, run_to_failure, write_config};
use crate::shared::{TOKEN, rtmp_listener_config};

#[cfg(unix)]
#[test]
fn built_process_does_not_unlink_an_existing_unix_listener_path() {
    let directory = TempDir::new().expect("Unix bind failure case");
    let path = directory.path().join("listener.sock");
    fs::write(&path, b"operator-owned").expect("existing Unix path");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(
        &config_path,
        &rtmp_listener_config(ListenerBind::Unix {
            path: path.clone(),
            mode: None,
        })
        .validate()
        .expect("valid Unix RTMP listener config"),
    );

    let failure = run_to_failure(&config_path, None);

    assert!(!failure.status.success());
    assert_eq!(
        fs::read(path).expect("existing path retained"),
        b"operator-owned"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn built_process_activates_a_real_unix_listener() {
    let directory = TempDir::new().expect("Unix listener case");
    let socket_path = directory.path().join("listener.sock");
    let config = rtmp_listener_config(ListenerBind::Unix {
        path: socket_path.clone(),
        mode: None,
    });
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        None,
    );

    server.wait_for_unix(&socket_path).await;

    assert!(socket_path.exists());
    server.shutdown();
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one process test covers validation, save, active preservation, and restart application"
)]
async fn unix_listener_mode_change_is_saved_as_restart_required_without_mutating_active_socket() {
    let directory = TempDir::new().expect("Unix mode reload case");
    let socket_path = directory.path().join("listener.sock");
    let management_address = reserve_tcp_address();
    let mut active = rtmp_listener_config(ListenerBind::Unix {
        path: socket_path.clone(),
        mode: Some(0o600),
    });
    active.management = Some(Management {
        bind: management_address,
        ui_dir: None,
        token_file: None,
    });
    let mut server = ServerProcess::start(
        &active.clone().validate().expect("valid active config"),
        Some(TOKEN),
    );
    server.wait_for_unix(&socket_path).await;
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let snapshot = http_request(
        management_address,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    let active_revision = snapshot["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();

    let mut candidate = active.clone();
    candidate.listeners[0].bind = ListenerBind::Unix {
        path: socket_path.clone(),
        mode: Some(0o660),
    };
    candidate.listeners[0].max_connections = Some(7);
    let body = serde_json::to_vec(&json!({ "config": candidate.clone() })).unwrap();
    let validated = http_request(
        management_address,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(validated.status, 200, "{}", validated.text());
    let validated = validated.json();
    assert_eq!(validated["restartRequired"], true);
    let direct_diagnostic = validated["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "I_RESTART_REQUIRED")
        .expect("direct restart diagnostic");
    assert_eq!(direct_diagnostic["path"], "/config/listeners");
    assert_eq!(
        direct_diagnostic["message"],
        "an active Unix listener mode changed; the saved configuration takes effect after a process restart"
    );
    assert!(
        validated["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| diagnostics
                .iter()
                .any(|diagnostic| diagnostic["code"] == "I_RESTART_REQUIRED"))
    );
    assert_eq!(
        fs::metadata(&socket_path)
            .expect("validated active Unix socket")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let saved = http_request(
        management_address,
        "PUT",
        "/api/v1/config",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
            (
                "If-Config-Revision",
                snapshot["diskRevision"].as_str().expect("disk revision"),
            ),
        ],
        &body,
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.text());
    let saved = saved.json();
    assert_eq!(saved["outcome"], "saved_restart_required");
    assert_eq!(saved["activationState"], "restart_required");
    assert_eq!(saved["restartRequired"], true);
    let direct_diagnostic = saved["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "I_RESTART_REQUIRED")
        .expect("saved direct restart diagnostic");
    assert_eq!(direct_diagnostic["path"], "/config/listeners");
    assert_eq!(
        direct_diagnostic["message"],
        "an active Unix listener mode changed; the saved configuration takes effect after a process restart"
    );
    assert_eq!(saved["activeRevision"], active_revision);
    assert!(saved["diagnostics"].as_array().is_some_and(|diagnostics| {
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "I_RESTART_REQUIRED")
    }));
    assert_eq!(
        fs::metadata(&socket_path)
            .expect("active Unix socket")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = http_request(
        management_address,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert_eq!(status["generation"]["activeRevision"], active_revision);
    assert_eq!(status["generation"]["quarantinedRevision"], Value::Null);
    assert_eq!(status["generation"]["lastFailure"], Value::Null);
    assert_eq!(status["generation"]["degraded"], false);
    let ready = http_request(management_address, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200, "{}", ready.text());
    server.shutdown();

    let mut restarted = ServerProcess::start(
        &candidate.validate().expect("valid restart candidate"),
        Some(TOKEN),
    );
    restarted.wait_for_unix(&socket_path).await;
    assert_eq!(
        fs::metadata(&socket_path)
            .expect("restarted Unix socket")
            .permissions()
            .mode()
            & 0o777,
        0o660
    );
    restarted.shutdown();
}
