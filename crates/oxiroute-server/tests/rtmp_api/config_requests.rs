#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn management_token_files_are_bounded_regular_nofollow_and_owner_only() {
    let directory = TempDir::new().expect("token directory");
    let config_path = directory.path().join("oxiroute.lua");
    let config = editable_config();
    fs::write(
        &config_path,
        render_lua(&config.validate().unwrap()).unwrap(),
    )
    .unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&config_path).unwrap();
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("config must load")
    };
    let token_path = write_file_with_mode(
        directory.path(),
        "management.token",
        format!("{TEST_TOKEN}\n").as_bytes(),
        0o600,
    );

    assert!(
        RtmpManagementApi::new(empty_control(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.effective_revision.clone(),
                &token_path,
                RuntimeMode::Direct,
            )
            .is_ok()
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        RtmpManagementApi::new(empty_control(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.effective_revision.clone(),
                &token_path,
                RuntimeMode::Direct,
            )
            .is_err()
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
    let token_link = directory.path().join("management-token-link");
    symlink(&token_path, &token_link).unwrap();
    assert!(
        RtmpManagementApi::new(empty_control(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator.clone(),
                document.effective_revision.clone(),
                &token_link,
                RuntimeMode::Direct,
            )
            .is_err()
    );

    fs::write(&token_path, format!("{TEST_TOKEN}\n\n")).unwrap();
    assert!(
        RtmpManagementApi::new(empty_control(), RuntimeMetrics::new(), empty_topology())
            .with_config_coordinator_from_token_file(
                coordinator,
                document.effective_revision,
                &token_path,
                RuntimeMode::Direct,
            )
            .is_err()
    );
}

#[tokio::test]
pub(super) async fn config_writes_require_a_current_revision_and_return_authoritative_conflicts() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let candidate = candidate_config(&active, "submitted-ingest");
    let request = serde_json::json!({ "config": candidate });

    let missing = harness
        .request("PUT", "/api/v1/config", None, Some(&request))
        .await;
    assert_eq!(missing.status, 428);
    assert_eq!(missing.json()["error"]["code"], "precondition_required");
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active.clone().validate().unwrap())
            .unwrap()
            .as_bytes()
    );

    let authoritative = candidate_config(&active, "external-ingest");
    fs::write(
        &harness.config_path,
        render_lua(&authoritative.clone().validate().unwrap())
            .expect("authoritative config renders"),
    )
    .expect("replace canonical config externally");
    let conflict = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&request),
        )
        .await;

    assert_eq!(conflict.status, 409);
    let conflict_json = conflict.json();
    assert_eq!(conflict_json["outcome"], "conflict");
    assert_eq!(
        conflict_json["activeRevision"],
        harness.active_revision.as_str()
    );
    assert_eq!(
        conflict_json["config"]["listeners"][0]["name"],
        "external-ingest"
    );
    assert_ne!(
        conflict_json["diskRevision"],
        harness.active_revision.as_str()
    );
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&authoritative.validate().unwrap())
            .unwrap()
            .as_bytes()
    );
}

#[tokio::test]
pub(super) async fn serves_authenticated_bounded_native_import_reports_without_exposing_paths() {
    let directory = TempDir::new().expect("temporary Apache source");
    let apache_root = directory.path().join("httpd.conf");
    fs::write(
        &apache_root,
        b"Listen 127.0.0.1:18080\n<VirtualHost 127.0.0.1:18080>\n  ServerName app.example.test\n  ProxyPass / http://127.0.0.1:9000/\n</VirtualHost>\n",
    )
    .expect("Apache source");
    let source = format!("apache_server {apache_root:?}\n");
    let harness = ManagementHarness::start_source(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
        "kdl",
        source.as_bytes(),
    )
    .await;

    let unauthorized = harness
        .request_with("GET", "/api/v1/import-reports", None, None, None, None)
        .await;
    assert_eq!(unauthorized.status, 401);

    let list = harness
        .request("GET", "/api/v1/import-reports", None, None)
        .await;
    assert_eq!(list.status, 200);
    assert_eq!(list.json()["reports"].as_array().map(Vec::len), Some(1));
    assert_eq!(list.json()["selection"], Value::Null);
    assert_eq!(list.json()["report"], Value::Null);

    let selected = harness
        .request("GET", "/api/v1/import-reports/0", None, None)
        .await;
    assert_eq!(selected.status, 200);
    let selected_json = selected.json();
    assert_eq!(selected_json["report"]["source"]["product"], "apache");
    assert!(
        selected_json["report"]["sourceGraph"]["sources"][0]["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("source-"))
    );
    assert_eq!(selected_json["preview"]["format"], "kdl");
    assert!(selected.body().len() <= 2 * MAX_CANONICAL_CONFIG_BYTES);

    let selected_text = String::from_utf8_lossy(selected.body());
    assert!(!selected_text.contains(apache_root.to_string_lossy().as_ref()));
    assert!(!selected_text.contains("httpd.conf"));

    let missing = harness
        .request("GET", "/api/v1/import-reports/1", None, None)
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json()["error"]["code"], "import_report_not_found");
}

#[tokio::test]
pub(super) async fn config_api_reports_source_format_composition_and_native_preview_name() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    let get = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(get.status, 200);
    let get = get.json();
    assert_eq!(get["configFormat"], "lua");
    assert_eq!(get["compositional"], false);
    assert_eq!(get["dependencyCount"], 0);
    assert_eq!(get["configPreview"], get["luaPreview"]);
    assert_eq!(get["candidateRevision"], get["activeRevision"]);
    assert_ne!(get["diskRevision"], get["candidateRevision"]);

    let request = serde_json::json!({ "config": active });
    let validated = harness
        .request("POST", "/api/v1/config/validate", None, Some(&request))
        .await;
    assert_eq!(validated.status, 200);
    let validated = validated.json();
    assert_eq!(validated["configFormat"], "lua");
    assert_eq!(validated["configPreview"], validated["luaPreview"]);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
pub(super) async fn config_api_preserves_rtmp_token_secrets_across_redacted_round_trip() {
    let mut active = editable_config();
    active.rtmp_services = vec![RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        max_inbound_message_size: 8 * 1024 * 1024,
        ack_window_size: 5_000_000,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "broadcast".into(),
            live: true,
            idle_streams: true,
            publish: RtmpAccessPolicy {
                rules: Vec::new(),
                token: Some(RtmpTokenPolicy {
                    source: RtmpTokenSource::StreamQuery,
                    parameter: "token".into(),
                    secret: "super-secret-token".into(),
                }),
            },
            play: RtmpAccessPolicy {
                rules: Vec::new(),
                token: Some(RtmpTokenPolicy {
                    source: RtmpTokenSource::StreamQuery,
                    parameter: "viewer".into(),
                    secret: "play-secret-token".into(),
                }),
            },
            limits: RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: Vec::new(),
        }],
    }];
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    let get = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(get.status, 200);
    let get_json = get.json();
    assert_eq!(
        get_json["config"]["rtmp_services"][0]["applications"][0]["publish"]["token"]["secret"],
        "<redacted>"
    );
    assert_eq!(
        get_json["config"]["rtmp_services"][0]["applications"][0]["play"]["token"]["secret"],
        "<redacted>"
    );
    assert!(get_json["configPreview"].as_str().is_some_and(|preview| {
        preview.contains("<redacted>")
            && !preview.contains("super-secret-token")
            && !preview.contains("play-secret-token")
    }));
    assert!(!String::from_utf8_lossy(&get.body).contains("super-secret-token"));
    assert!(!String::from_utf8_lossy(&get.body).contains("play-secret-token"));

    let mut round_trip: ConfigDraft = serde_json::from_value(get_json["config"].clone())
        .expect("redacted config remains a typed request");
    round_trip.rtmp_services[0].outbound_chunk_size = 8_192;
    let request = serde_json::json!({ "config": round_trip });
    let validated = harness
        .request("POST", "/api/v1/config/validate", None, Some(&request))
        .await;
    assert_eq!(validated.status, 200);
    let validated_json = validated.json();
    assert_eq!(
        validated_json["normalizedConfig"]["rtmp_services"][0]["applications"][0]["publish"]["token"]
            ["secret"],
        "<redacted>"
    );
    assert_eq!(
        validated_json["normalizedConfig"]["rtmp_services"][0]["applications"][0]["play"]["token"]
            ["secret"],
        "<redacted>"
    );
    assert!(!String::from_utf8_lossy(&validated.body).contains("super-secret-token"));
    assert!(!String::from_utf8_lossy(&validated.body).contains("play-secret-token"));

    let normalized = serde_json::json!({
        "config": validated_json["normalizedConfig"].clone(),
    });
    let saved = harness
        .request(
            "PUT",
            "/api/v1/config",
            get_json["diskRevision"].as_str(),
            Some(&normalized),
        )
        .await;
    assert_eq!(saved.status, 200);
    assert!(!String::from_utf8_lossy(&saved.body).contains("super-secret-token"));
    assert!(!String::from_utf8_lossy(&saved.body).contains("play-secret-token"));

    let persisted = fs::read_to_string(&harness.config_path).expect("saved configuration");
    assert!(persisted.contains("super-secret-token"));
    assert!(persisted.contains("play-secret-token"));
    assert!(persisted.contains("outbound_chunk_size = 8192"));

    let after_save = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(after_save.status, 200);
    assert_eq!(
        after_save.json()["config"]["rtmp_services"][0]["applications"][0]["publish"]["token"]["secret"],
        "<redacted>"
    );
    assert_eq!(
        after_save.json()["config"]["rtmp_services"][0]["applications"][0]["play"]["token"]["secret"],
        "<redacted>"
    );
    assert!(!String::from_utf8_lossy(&after_save.body).contains("super-secret-token"));
    assert!(!String::from_utf8_lossy(&after_save.body).contains("play-secret-token"));

    let before_unresolved = fs::read(&harness.config_path).expect("saved configuration");
    round_trip.rtmp_services[0].name = "renamed".into();
    let unresolved = harness
        .request(
            "PUT",
            "/api/v1/config",
            after_save.json()["diskRevision"].as_str(),
            Some(&serde_json::json!({ "config": round_trip })),
        )
        .await;
    assert_eq!(unresolved.status, 422);
    assert_eq!(
        unresolved.json()["error"]["code"],
        "redacted_rtmp_token_unresolved"
    );
    assert_eq!(
        fs::read(&harness.config_path).expect("configuration after rejected save"),
        before_unresolved
    );
}

#[tokio::test]
pub(super) async fn config_api_rejects_typed_save_over_a_compositional_root() {
    let config = editable_config();
    let rendered = serde_json::json!({
        "templates": { "base": serde_json::to_value(&config).unwrap() },
        "use": "base",
    });
    let source = serde_json::to_vec_pretty(&rendered).unwrap();
    let harness = ManagementHarness::start_source(
        &(config.clone())
            .validate()
            .expect("valid management fixture"),
        "hocon",
        &source,
    )
    .await;
    let before = fs::read(&harness.config_path).unwrap();
    let get = harness.request("GET", "/api/v1/config", None, None).await;
    let snapshot = get.json();
    assert_eq!(snapshot["configFormat"], "hocon");
    assert_eq!(snapshot["compositional"], true);

    let request = serde_json::json!({ "config": config });
    let response = harness
        .request(
            "PUT",
            "/api/v1/config",
            snapshot["diskRevision"].as_str(),
            Some(&request),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(
        response.json()["diagnostics"][0]["code"],
        "E_COMPOSITIONAL_ROOT"
    );
    assert_eq!(fs::read(&harness.config_path).unwrap(), before);
}

#[tokio::test]
pub(super) async fn rejects_invalid_malformed_and_oversized_config_requests() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let mut invalid = active.clone();
    invalid.version = 2;
    let invalid_request = serde_json::json!({ "config": invalid });

    let validation = harness
        .request(
            "POST",
            "/api/v1/config/validate",
            None,
            Some(&invalid_request),
        )
        .await;
    assert_eq!(validation.status, 422);
    assert_eq!(validation.json()["diagnostics"][0]["severity"], "error");
    assert_eq!(validation.json()["diagnostics"][0]["path"], "/version");
    assert!(
        validation.json()["diagnostics"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("version 2"))
    );

    let mut serviceless = active.clone();
    serviceless.listeners.push(Listener {
        name: "edge".into(),
        bind: socket_bind(loopback_address(1935)),
        protocol: Protocol::Rtmp,
        service: None,
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    let serviceless = harness
        .request(
            "POST",
            "/api/v1/config/validate",
            None,
            Some(&serde_json::json!({ "config": serviceless })),
        )
        .await;
    assert_eq!(serviceless.status, 422);
    assert_eq!(
        serviceless.json()["diagnostics"][0]["path"],
        "/listeners/edge/service"
    );
    assert!(
        serviceless.json()["diagnostics"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("listener `edge`"))
    );

    let misspelled = harness
        .request(
            "POST",
            "/api/v1/config/validate",
            None,
            Some(&serde_json::json!({ "config": { "version": 1, "listners": [] } })),
        )
        .await;
    assert_eq!(misspelled.status, 422);
    assert_eq!(
        misspelled.json()["diagnostics"][0]["code"],
        "E_INVALID_FIELD"
    );
    assert!(
        misspelled.json()["diagnostics"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("`listners`"))
    );

    let save = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(harness.disk_revision.as_str()),
            Some(&invalid_request),
        )
        .await;
    assert_eq!(save.status, 422);
    assert_eq!(
        fs::read(&harness.config_path).unwrap(),
        render_lua(&active.clone().validate().unwrap())
            .unwrap()
            .as_bytes()
    );

    rejects_malformed_and_oversized_validation_bodies(&harness).await;
}

async fn rejects_malformed_and_oversized_validation_bodies(harness: &ManagementHarness) {
    let malformed = harness
        .raw_request(
            format!(
                "POST /api/v1/config/validate HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{{"
            )
            .into_bytes(),
        )
        .await;
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.json()["error"]["code"], "malformed_json");

    let oversized = harness
        .raw_request(
            format!(
                "POST /api/v1/config/validate HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_CANONICAL_CONFIG_BYTES + 1
            )
            .into_bytes(),
        )
        .await;
    assert_eq!(oversized.status, 413);
    assert_eq!(oversized.json()["error"]["code"], "config_body_too_large");
}

#[tokio::test]
pub(super) async fn config_routes_reject_wrong_methods_and_paths() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    let config_method = harness.request("POST", "/api/v1/config", None, None).await;
    assert_eq!(config_method.status, 405);
    assert_eq!(
        config_method.headers.get("allow").map(String::as_str),
        Some("GET, PUT")
    );

    let validate_method = harness
        .request("GET", "/api/v1/config/validate", None, None)
        .await;
    assert_eq!(validate_method.status, 405);
    assert_eq!(
        validate_method.headers.get("allow").map(String::as_str),
        Some("POST")
    );

    let missing = harness
        .request("GET", "/api/v1/config/extra", None, None)
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json()["error"]["code"], "route_not_found");
}

#[tokio::test]
pub(super) async fn config_routes_require_exact_paths_json_media_and_raw_revision_headers() {
    let active = editable_config();
    let harness = ManagementHarness::start(
        &(active.clone())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let request = serde_json::json!({ "config": candidate_config(&active, "candidate") });

    for path in [
        "/api/v1/config/",
        "/api//v1/config",
        "/api/v1/config/validate/",
        "/api/v1//config/validate",
    ] {
        assert_eq!(harness.request("GET", path, None, None).await.status, 404);
    }

    for content_type in [None, Some("text/plain")] {
        let response = harness
            .request_with(
                "POST",
                "/api/v1/config/validate",
                Some(TEST_TOKEN),
                None,
                content_type,
                Some(&request),
            )
            .await;
        assert_eq!(response.status, 415);
        assert_eq!(response.json()["error"]["code"], "unsupported_media_type");
    }
    assert_eq!(
        harness
            .request_with(
                "POST",
                "/api/v1/config/validate",
                Some(TEST_TOKEN),
                None,
                Some("application/json; charset=utf-8"),
                Some(&request),
            )
            .await
            .status,
        200
    );

    let missing_revision = harness
        .request("PUT", "/api/v1/config", None, Some(&request))
        .await;
    assert_eq!(missing_revision.status, 428);
    let malformed_revision = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(&format!("\"{}\"", harness.active_revision.as_str())),
            Some(&request),
        )
        .await;
    assert_eq!(malformed_revision.status, 400);
    assert_eq!(
        malformed_revision.json()["error"]["code"],
        "invalid_config_revision"
    );
    let uppercase_revision = harness
        .request(
            "PUT",
            "/api/v1/config",
            Some(&harness.disk_revision.as_str().to_ascii_uppercase()),
            Some(&request),
        )
        .await;
    assert_eq!(uppercase_revision.status, 200);

    let legacy_header = harness
        .raw_request(
            format!(
                "PUT /api/v1/config HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nIf-Match: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                harness.active_revision.as_str(),
                request.to_string().len(),
                request,
            )
            .into_bytes(),
        )
        .await;
    assert_eq!(legacy_header.status, 428);
}
