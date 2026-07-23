#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use oxiroute_config::{
    Config, Listener, ListenerBind, Management, Protocol, RtmpApplication, RtmpRecorderStart,
    RtmpService,
};
use serde_json::json;
use tempfile::TempDir;

use config_support::{empty_config, rtmp_recorder_with_queue_bytes, socket_bind};
use http_support::http_request;
use process_support::{
    ServerProcess, build_ui, output_text, reserve_tcp_address, run_to_failure, write_config,
    write_token,
};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn built_management_ui_and_authenticated_config_lifecycle_run_over_real_tcp() {
    let ui_dir = build_ui();
    let management_address = reserve_tcp_address();
    let active = management_config(management_address, Some(ui_dir.clone()));
    let mut server = ServerProcess::start(&active, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let token_path = server.token_path.as_ref().expect("management token file");
    assert_eq!(
        fs::metadata(token_path)
            .expect("token metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let index = http_request(management_address, "GET", "/", &[], &[]).await;
    assert_eq!(index.status, 200);
    assert_eq!(
        index.headers.get("content-type").map(String::as_str),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(index.body, fs::read(ui_dir.join("index.html")).unwrap());
    let asset_paths = built_asset_paths(&index.body);
    assert!(!asset_paths.is_empty(), "built index must reference assets");
    for path in asset_paths {
        let response = http_request(management_address, "GET", &path, &[], &[]).await;
        assert_eq!(response.status, 200, "asset {path}");
        assert_eq!(
            response.body,
            fs::read(ui_dir.join(path.trim_start_matches('/'))).unwrap(),
            "asset {path}"
        );
    }

    let monitoring = http_request(
        management_address,
        "GET",
        "/api/v1/monitoring",
        &[("Cache-Control", "no-store")],
        &[],
    )
    .await;
    assert_eq!(monitoring.status, 200);
    let monitoring = monitoring.json();
    assert_eq!(monitoring["listeners"], json!([]));
    assert_eq!(monitoring["upstreamPools"], json!([]));
    assert_eq!(monitoring["certbotCertificates"], json!([]));
    assert!(monitoring["certbotWatcher"].is_null());
    assert_eq!(
        monitoring["rtmp"],
        json!({
            "activeStreams": 0,
            "publishers": 0,
            "subscribers": 0,
            "mediaPayloadBytesReceived": 0,
            "recordingSupported": false,
            "manualRecording": false,
            "recorderBytesWritten": 0,
            "recorderSegmentsStarted": 0,
            "recorderSegmentsCompleted": 0,
            "recorderDiscontinuities": 0,
            "recorders": [],
        })
    );

    let catalog = http_request(management_address, "GET", "/api/v1/rtmp/streams", &[], &[])
        .await
        .json();
    assert!(catalog["as_of_unix_ms"].as_u64().is_some());
    assert_eq!(catalog["revision"], "0");
    assert_eq!(
        catalog["capabilities"],
        json!({ "live_ingest": false, "manual_recording": false })
    );
    assert_eq!(catalog["streams"], json!([]));

    let topology = http_request(
        management_address,
        "GET",
        "/api/v1/topology",
        &[("Cache-Control", "no-store")],
        &[],
    )
    .await
    .json();
    assert_eq!(topology["schemaVersion"], 1);
    assert_eq!(topology["state"]["config"], "active");
    assert_eq!(topology["state"]["runtime"], "active");
    assert_eq!(topology["nodes"], json!([]));
    assert_eq!(topology["edges"], json!([]));
    assert_eq!(topology["overlays"], json!([]));

    let unauthorized = http_request(management_address, "GET", "/api/v1/config", &[], &[]).await;
    assert_eq!(unauthorized.status, 401);
    assert_eq!(unauthorized.json()["error"]["code"], "unauthorized");

    let authorization = format!("Bearer {TOKEN}");
    let get = http_request(
        management_address,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(get.status, 200);
    let snapshot = get.json();
    assert_eq!(snapshot["schemaVersion"], 1);
    assert_eq!(snapshot["config"], serde_json::to_value(&active).unwrap());
    assert_eq!(snapshot["diskRevision"], snapshot["activeRevision"]);
    assert!(!String::from_utf8_lossy(&get.body).contains(TOKEN));

    let missing_root = TempDir::new()
        .expect("secret recording parent")
        .path()
        .join("tenant-secret-recording-root");
    let invalid_candidate = recording_candidate(&active, &missing_root);
    let invalid_body = serde_json::to_vec(&json!({ "config": invalid_candidate })).unwrap();
    let rejected = http_request(
        management_address,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &invalid_body,
    )
    .await;
    assert_eq!(rejected.status, 422);
    assert_eq!(
        rejected.json()["diagnostics"][0]["code"],
        "E_RUNTIME_PREPARE"
    );
    let rejected_wire = String::from_utf8_lossy(&rejected.body);
    assert!(
        rejected_wire.contains("candidate cannot be prepared as a complete runtime generation")
    );
    assert!(!rejected_wire.contains(&missing_root.display().to_string()));
    assert!(!rejected_wire.contains(TOKEN));

    let mut candidate = active.clone();
    candidate.management.as_mut().unwrap().ui_dir = None;
    let candidate_body = serde_json::to_vec(&json!({ "config": candidate })).unwrap();
    let validated = http_request(
        management_address,
        "POST",
        "/api/v1/config/validate",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &candidate_body,
    )
    .await;
    assert_eq!(validated.status, 200);
    assert_eq!(
        validated.json()["normalizedConfig"],
        serde_json::to_value(&candidate).unwrap()
    );

    let revision = snapshot["diskRevision"].as_str().unwrap();
    let saved = http_request(
        management_address,
        "PUT",
        "/api/v1/config",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
            ("If-Config-Revision", revision),
        ],
        &candidate_body,
    )
    .await;
    assert_eq!(saved.status, 200);
    let saved = saved.json();
    assert_eq!(saved["activeRevision"], snapshot["activeRevision"]);
    assert_eq!(saved["outcome"], "saved_restart_required");
    assert_eq!(saved["activationState"], "restart_required");
    assert_eq!(saved["restartRequired"], true);

    let persisted = http_request(
        management_address,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    assert_eq!(
        persisted["config"],
        serde_json::to_value(candidate).unwrap()
    );
    assert_eq!(persisted["diskRevision"], saved["diskRevision"]);
    assert_eq!(persisted["activeRevision"], snapshot["activeRevision"]);
    assert!(
        fs::read_to_string(&server.config_path)
            .expect("persisted process config")
            .contains("ui_dir = nil")
    );
    server.shutdown();
}

#[test]
fn built_process_rejects_invalid_token_config_and_recording_roots() {
    let token_case = TempDir::new().expect("invalid token case");
    let token_config_path = token_case.path().join("oxiroute.lua");
    let token_config = management_config(reserve_tcp_address(), None);
    write_config(&token_config_path, &token_config);
    let token_path = write_token(token_case.path(), TOKEN, 0o644);
    let token_failure = run_to_failure(&token_config_path, Some(&token_path));
    assert!(!token_failure.status.success());
    let token_output = output_text(&token_failure);
    assert!(
        token_output.contains("management token file mode must be 0400 or 0600"),
        "unexpected token failure: {token_output}"
    );
    assert!(!token_output.contains(TOKEN));
    assert!(!token_output.contains(&token_path.display().to_string()));

    let config_case = TempDir::new().expect("invalid config case");
    let invalid_config_path = config_case.path().join("invalid.lua");
    fs::write(&invalid_config_path, "return {").unwrap();
    let config_failure = run_to_failure(&invalid_config_path, None);
    assert!(!config_failure.status.success());
    let config_output = output_text(&config_failure);
    assert!(config_output.contains("canonical configuration was rejected"));

    let recording_case = TempDir::new().expect("invalid recording case");
    let recording_config_path = recording_case.path().join("oxiroute.lua");
    let secret_root = recording_case.path().join("missing-secret-recording-root");
    let recording_config = recording_candidate(&empty_config(), &secret_root);
    write_config(&recording_config_path, &recording_config);
    let recording_failure = run_to_failure(&recording_config_path, None);
    assert!(!recording_failure.status.success());
    let recording_output = output_text(&recording_failure);
    assert!(recording_output.contains("failed recording-root preflight"));
    assert!(!recording_output.contains(&secret_root.display().to_string()));
}

#[test]
fn built_process_fails_before_runtime_when_a_tcp_listener_cannot_bind() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let address = occupied.local_addr().expect("occupied address");
    let directory = TempDir::new().expect("bind failure case");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(&config_path, &rtmp_listener_config(socket_bind(address)));

    let failure = run_to_failure(&config_path, None);

    assert!(!failure.status.success());
    assert!(output_text(&failure).contains("could not bind socket"));
}

#[cfg(unix)]
#[test]
fn built_process_does_not_unlink_an_existing_unix_listener_path() {
    let directory = TempDir::new().expect("Unix bind failure case");
    let path = directory.path().join("listener.sock");
    fs::write(&path, b"operator-owned").expect("existing Unix path");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(
        &config_path,
        &rtmp_listener_config(ListenerBind::Unix { path: path.clone() }),
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
    });
    let mut server = ServerProcess::start(&config, None);

    server.wait_for_unix(&socket_path).await;

    assert!(socket_path.exists());
    server.shutdown();
}

fn management_config(
    management_address: std::net::SocketAddr,
    ui_dir: Option<std::path::PathBuf>,
) -> Config {
    Config {
        management: Some(Management {
            bind: management_address,
            ui_dir,
        }),
        ..empty_config()
    }
}

fn recording_candidate(active: &Config, root: &Path) -> Config {
    let mut candidate = active.clone();
    candidate.listeners.push(Listener {
        name: "recording-wire".into(),
        bind: socket_bind(reserve_tcp_address()),
        protocol: Protocol::Rtmp,
        service: Some("recording-wire".into()),
        tls_profile: None,
        max_connections: Some(8),
    });
    candidate.rtmp_services.push(RtmpService {
        name: "recording-wire".into(),
        applications: vec![RtmpApplication {
            name: "live".into(),
            live: true,
            idle_streams: false,
            recorders: vec![rtmp_recorder_with_queue_bytes(
                "archive",
                RtmpRecorderStart::Continuous,
                root,
                1024 * 1024,
            )],
        }],
    });
    candidate
}

fn rtmp_listener_config(bind: ListenerBind) -> Config {
    Config {
        listeners: vec![Listener {
            name: "live".into(),
            bind,
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            max_connections: Some(8),
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: false,
                recorders: Vec::new(),
            }],
        }],
        ..empty_config()
    }
}

fn built_asset_paths(index: &[u8]) -> Vec<String> {
    let index = std::str::from_utf8(index).expect("built index UTF-8");
    let mut paths = Vec::new();
    for marker in ["src=\"", "href=\""] {
        let mut remainder = index;
        while let Some(start) = remainder.find(marker) {
            remainder = &remainder[start + marker.len()..];
            let end = remainder.find('"').expect("asset attribute terminator");
            let path = &remainder[..end];
            if path.starts_with("/assets/") {
                paths.push(path.to_owned());
            }
            remainder = &remainder[end + 1..];
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
