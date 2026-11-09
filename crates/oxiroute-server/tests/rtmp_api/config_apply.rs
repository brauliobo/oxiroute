#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn direct_put_preflight_failure_does_not_mutate_disk() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut candidate = active.clone();
    let CertificateSource::Files {
        private_key_path, ..
    } = &mut candidate.certificates[0].source
    else {
        panic!("direct-file certificate")
    };
    *private_key_path = fixture("proxy-b-key.pem");
    let request = serde_json::json!({ "config": candidate });

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(
        response.json()["diagnostics"][0]["code"],
        "E_RUNTIME_PREPARE"
    );
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active.clone().validate().unwrap())
            .unwrap()
            .as_bytes()
    );
}

#[cfg(unix)]
#[tokio::test]
pub(super) async fn candidate_recorder_preflight_never_mutates_the_recording_root() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let root = TempDir::new().expect("recording root");
    let mut candidate = candidate_config(&active, "candidate-recorder");
    let mut recorder = rtmp_recorder("archive", RtmpRecorderStart::Continuous, root.path());
    recorder.suffix_template = "-%Y-private.flv".into();
    candidate.rtmp_services[0].applications[0]
        .recorders
        .push(recorder);
    let request = serde_json::json!({ "config": candidate });

    let validation = harness
        .request("POST", "/api/v1/config/validate", None, Some(&request))
        .await;
    assert_eq!(validation.status, 200);
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
    let validation_json = validation.json();
    let topology = validation_json["topology"].to_string();
    assert!(!topology.contains(&root.path().display().to_string()));
    assert!(!topology.contains("private.flv"));
    let listener = validation_json["topology"]["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["kind"] == "rtmp_listener"))
        .expect("candidate RTMP listener");
    assert_eq!(
        listener["attributes"]["applications"][0]["recording"]["recorderCount"],
        1
    );
    assert_eq!(
        listener["attributes"]["applications"][0]["recording"]["continuousRecorderCount"],
        1
    );

    let saved = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(saved.status, 200);
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
    assert!(!root.path().join(".oxiroute-recording.lock").exists());
}

#[tokio::test]
pub(super) async fn missing_candidate_ui_assets_are_rejected_without_mutating_disk() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut candidate = active.clone();
    candidate.management = Some(Management {
        bind: "127.0.0.1:9090".parse().unwrap(),
        ui_dir: Some(harness.config_path.with_file_name("missing-ui")),
    });
    let request = serde_json::json!({ "config": candidate });

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(response.json()["diagnostics"][0]["code"], "E_UI_ASSETS");
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active.clone().validate().unwrap())
            .unwrap()
            .as_bytes()
    );
}

#[tokio::test]
pub(super) async fn saving_the_active_generation_is_truthfully_idempotent() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let request = serde_json::json!({ "config": active });

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 200);
    let response_json = response.json();
    assert_eq!(response_json["outcome"], "unchanged_active");
    assert_eq!(response_json["activationState"], "active");
    assert_eq!(response_json["restartRequired"], false);
    assert_eq!(
        response_json["candidateRevision"],
        response_json["activeRevision"]
    );
    assert_ne!(
        response_json["diskRevision"],
        response_json["activeRevision"]
    );
}

#[tokio::test]
pub(super) async fn config_api_applies_an_added_listener_in_both_runtime_modes_without_a_restart() {
    for (mode, restart_required, outcome, activation_state, diagnostic) in [
        (
            RuntimeMode::Direct,
            false,
            "saved_pending_activation",
            "pending",
            "I_ACTIVATION_PENDING",
        ),
        (
            RuntimeMode::Supervised,
            false,
            "saved_pending_activation",
            "pending",
            "I_ACTIVATION_PENDING",
        ),
    ] {
        let active = editable_config();
        let validated = active.clone().validate().expect("valid active config");
        let source = render_lua(&validated).expect("active config renders");
        let harness = ManagementHarness::start_source_with_generation_manager(
            &validated,
            "lua",
            source.as_bytes(),
            true,
            mode,
        )
        .await;
        let candidate = candidate_config(&active, "candidate");
        let request = serde_json::json!({ "config": candidate });

        let preview = harness
            .request("POST", "/api/v1/config/validate", None, Some(&request))
            .await;
        assert_eq!(preview.status, 200);
        assert_eq!(preview.json()["restartRequired"], restart_required);
        let candidate_bind = candidate
            .listeners
            .last()
            .expect("candidate listener")
            .bind
            .clone();
        let expected_added = serde_json::json!([format!("candidate (tcp {candidate_bind})")]);
        let preview_changes = preview.json()["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|diagnostic| diagnostic["code"] == "I_LISTENER_CHANGES")
            .cloned()
            .unwrap_or_else(|| panic!("{mode:?}: preview lists the listener changes"));
        assert_eq!(preview_changes["listeners"]["added"], expected_added);
        assert_eq!(
            preview_changes["listeners"]["removed"],
            serde_json::json!([])
        );
        assert_eq!(preview_changes["path"], "/config/listeners");

        let saved = harness
            .request(
                "PUT",
                "/api/v1/config",
                Some(harness.disk_revision.as_str()),
                Some(&request),
            )
            .await;
        assert_eq!(saved.status, 200);
        assert_eq!(saved.json()["outcome"], outcome);
        assert_eq!(saved.json()["activationState"], activation_state);
        assert_eq!(saved.json()["restartRequired"], restart_required);
        let diagnostics = saved.json()["diagnostics"].as_array().unwrap().clone();
        assert_eq!(diagnostics.last().unwrap()["code"], diagnostic);
        let saved_changes = diagnostics
            .iter()
            .find(|diagnostic| diagnostic["code"] == "I_LISTENER_CHANGES")
            .unwrap_or_else(|| panic!("{mode:?}: save lists the listener changes"));
        assert_eq!(saved_changes["listeners"]["added"], expected_added);
    }
}

#[tokio::test]
pub(super) async fn supervised_config_api_reports_descriptor_changes_as_listener_changes_not_restarts()
 {
    for change in ["bind", "protocol", "control"] {
        let mut active = candidate_config(&editable_config(), "active");
        let active_address = reserve_tcp_address();
        active.listeners[0].bind = socket_bind(active_address);
        let validated = active.clone().validate().expect("valid active config");
        let source = render_lua(&validated).expect("active config renders");
        let harness = ManagementHarness::start_source_with_generation_manager(
            &validated,
            "lua",
            source.as_bytes(),
            true,
            RuntimeMode::Supervised,
        )
        .await;
        let mut candidate = active.clone();
        match change {
            "bind" => {
                candidate.listeners[0].bind = socket_bind(reserve_tcp_address());
            }
            "protocol" => {
                candidate.listeners[0].protocol = Protocol::Http;
                candidate.listeners[0].service = Some("web".into());
                candidate.http_services.push(
                    serde_json::from_value(serde_json::json!({
                        "name": "web",
                        "routes": [{
                            "path": {"kind": "segment_prefix", "value": "/"},
                            "action": {"type": "fixed_response", "status": 200}
                        }]
                    }))
                    .expect("HTTP service fixture"),
                );
            }
            "control" => {
                candidate.stats = Some(oxiroute_config::Stats {
                    binds: vec![reserve_tcp_address()],
                    admin_token_file: None,
                    pages: Vec::new(),
                });
            }
            _ => unreachable!(),
        }
        candidate
            .clone()
            .validate()
            .unwrap_or_else(|error| panic!("valid {change} candidate: {error}"));
        let request = serde_json::json!({ "config": candidate });

        for (method, path, revision) in [
            ("POST", "/api/v1/config/validate", None),
            (
                "PUT",
                "/api/v1/config",
                Some(harness.disk_revision.as_str()),
            ),
        ] {
            let response = harness
                .request(method, path, revision, Some(&request))
                .await;
            assert_eq!(response.status, 200, "{change}: {}", response.text());
            let response = response.json();
            assert_eq!(response["restartRequired"], false, "{change}");
            let diagnostics = response["diagnostics"].as_array().unwrap();
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic["code"] != "I_RESTART_REQUIRED"),
                "{change}: {diagnostics:?}"
            );
            let found = diagnostics
                .iter()
                .find(|diagnostic| diagnostic["code"] == "I_LISTENER_CHANGES");
            if change == "protocol" {
                // The socket keeps its address, so the worker re-serves it with the new protocol
                // and no listener is added or removed.
                assert!(found.is_none(), "{change}: {diagnostics:?}");
                continue;
            }
            let diagnostic =
                found.unwrap_or_else(|| panic!("{change}: listener changes diagnostic"));
            assert_eq!(diagnostic["path"], "/config/listeners");
            assert!(
                diagnostic["message"]
                    .as_str()
                    .unwrap()
                    .contains("the supervising master binds"),
                "{change}: {diagnostic}"
            );
            let listeners = &diagnostic["listeners"];
            match change {
                // The old address stops accepting and the new one is bound before the switch.
                "bind" => {
                    assert_eq!(listeners["added"].as_array().unwrap().len(), 1);
                    assert_eq!(listeners["removed"].as_array().unwrap().len(), 1);
                }
                "control" => {
                    assert_eq!(listeners["added"].as_array().unwrap().len(), 1);
                    assert!(listeners["removed"].as_array().unwrap().is_empty());
                }
                _ => unreachable!(),
            }
        }
    }
}

#[tokio::test]
pub(super) async fn conflict_reload_failure_returns_no_fabricated_revision() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let request = serde_json::json!({ "config": candidate_config(&active, "candidate") });
    fs::write(&harness.config_path, "not valid canonical Lua").unwrap();

    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 503);
    let response_json = response.json();
    assert!(response_json["diskRevision"].is_null());
    assert_eq!(response_json["outcome"], "authoritative_state_unavailable");
    assert_eq!(
        response_json["error"]["code"],
        "authoritative_config_unavailable"
    );
}

#[tokio::test]
pub(super) async fn chunked_config_body_is_bounded_while_streaming() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut request = format!(
        "POST /api/v1/config/validate HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    let chunk = vec![b'a'; 64 * 1024];
    for _ in 0..16 {
        request.extend_from_slice(b"10000\r\n");
        request.extend_from_slice(&chunk);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"1\r\na\r\n0\r\n\r\n");

    let response = harness.raw_request(request).await;
    assert_eq!(response.status, 413);
    assert_eq!(response.json()["error"]["code"], "config_body_too_large");
}

#[tokio::test]
pub(super) async fn config_snapshot_follows_every_change_to_the_persisted_file() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let original = fs::read(&harness.config_path).unwrap();
    let get = || harness.request("GET", "/api/v1/config", None, None);

    let first = get().await;
    assert_eq!(first.status, 200);
    assert_eq!(get().await.body, first.body);

    let edited = candidate_config(&active, "edited").validate().unwrap();
    fs::write(&harness.config_path, render_lua(&edited).unwrap()).unwrap();
    let changed = get().await;
    assert_eq!(changed.status, 200);
    assert_ne!(changed.json()["diskRevision"], first.json()["diskRevision"]);
    assert!(
        changed.json()["configPreview"]
            .as_str()
            .unwrap()
            .contains("edited")
    );

    fs::write(&harness.config_path, original).unwrap();
    assert_eq!(get().await.body, first.body);
}

async fn get_config(harness: &ManagementHarness, validator: Option<&str>) -> HttpResponse {
    let authorization = format!("Bearer {TEST_TOKEN}");
    let mut headers = vec![("Authorization", authorization.as_str())];
    headers.extend(validator.map(|validator| ("If-None-Match", validator)));
    http_request(harness.address, "GET", "/api/v1/config", &headers, b"").await
}

#[tokio::test]
pub(super) async fn config_snapshot_is_revalidated_with_the_revision_etag() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    let first = get_config(&harness, None).await;
    assert_eq!(first.status, 200);
    let etag = first.header("etag").expect("snapshot validator").to_owned();

    let revalidated = get_config(&harness, Some(&etag)).await;
    assert_eq!(revalidated.status, 304);
    assert!(revalidated.body.is_empty());
    assert_eq!(revalidated.header("etag"), Some(etag.as_str()));
    assert_eq!(get_config(&harness, Some("\"stale\"")).await.status, 200);

    let edited = candidate_config(&active, "edited").validate().unwrap();
    fs::write(&harness.config_path, render_lua(&edited).unwrap()).unwrap();
    let changed = get_config(&harness, Some(&etag)).await;
    assert_eq!(changed.status, 200);
    assert_ne!(changed.header("etag"), Some(etag.as_str()));
}
