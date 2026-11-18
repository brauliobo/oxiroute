use std::{fs, os::unix::fs::PermissionsExt as _, time::Duration};

use oxiroute_config::{ListenerBind, Management};
use serde_json::json;
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
async fn unix_listener_mode_change_is_applied_in_place_by_the_saved_configuration() {
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
    let active_revision = snapshot["activeRevision"].clone();

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
    assert_eq!(validated["restartRequired"], false);
    assert!(
        validated["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|diagnostic| diagnostic["code"] != "I_RESTART_REQUIRED")
    );
    assert_eq!(
        socket_mode(&socket_path),
        0o600,
        "validation changes nothing"
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
    assert_eq!(saved["restartRequired"], false);
    assert_eq!(saved["outcome"], "saved_pending_activation");

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let status = loop {
        let status = http_request(
            management_address,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .json();
        if status["generation"]["activeRevision"] != active_revision {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the saved mode did not activate"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(status["generation"]["degraded"], false);
    assert_eq!(socket_mode(&socket_path), 0o660);
    let ready = http_request(management_address, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200, "{}", ready.text());
    server.shutdown();
}

fn socket_mode(path: &std::path::Path) -> u32 {
    fs::metadata(path)
        .expect("Unix socket metadata")
        .permissions()
        .mode()
        & 0o777
}
