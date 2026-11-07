#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn config_routes_require_the_injected_bearer_token() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    for (method, path) in [
        ("GET", "/api/v1/config"),
        ("POST", "/api/v1/config/validate"),
        ("PUT", "/api/v1/config"),
        ("GET", "/api/v1/rtmp/stats"),
        ("GET", "/api/v1/audit"),
        ("GET", "/api/v1/audit/status"),
    ] {
        let response = harness
            .request_with(method, path, None, None, None, None)
            .await;
        assert_eq!(response.status, 401);
        assert_eq!(response.json()["error"]["code"], "unauthorized");
        assert_eq!(
            response.headers.get("www-authenticate").map(String::as_str),
            Some("Bearer")
        );
        assert!(!response.headers.contains_key("access-control-allow-origin"));
    }

    let wrong = harness
        .request_with(
            "GET",
            "/api/v1/config",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(wrong.status, 401);

    let authorized = harness.request("GET", "/api/v1/config", None, None).await;
    assert_eq!(authorized.status, 200);
    assert!(authorized.headers.contains_key("x-correlation-id"));
    assert_eq!(
        harness
            .request_with("GET", "/api/v1/topology", None, None, None, None)
            .await
            .status,
        401
    );
    assert_eq!(
        harness
            .request("GET", "/api/v1/topology", None, None)
            .await
            .status,
        200
    );
}

#[tokio::test]
pub(super) async fn protected_read_only_registry_preserves_auth_method_and_path_contract() {
    let active = editable_config()
        .validate()
        .expect("valid management fixture");
    let source = render_lua(&active).expect("active config renders");
    let harness = ManagementHarness::start_source_with_generation_manager(
        &active,
        "lua",
        source.as_bytes(),
        true,
        RuntimeMode::Direct,
    )
    .await;

    for path in [
        "/api/v1/status",
        "/api/v1/listeners",
        "/api/v1/pools",
        "/api/v1/servers",
        "/api/v1/generations",
    ] {
        let missing = harness
            .request_with("GET", path, None, None, None, None)
            .await;
        assert_eq!(missing.status, 401, "{path}");
        assert_eq!(missing.json()["error"]["code"], "unauthorized");

        let missing_method = harness
            .request_with("POST", path, None, None, None, None)
            .await;
        assert_eq!(missing_method.status, 401, "{path}");

        let wrong_method = harness
            .request_with("POST", path, Some(TEST_TOKEN), None, None, None)
            .await;
        assert_eq!(wrong_method.status, 405, "{path}");
        assert_eq!(wrong_method.header("allow"), Some("GET"));

        let authorized = harness.request("GET", path, None, None).await;
        assert_eq!(authorized.status, 200, "{path}");

        let queried = harness
            .request("GET", &format!("{path}?view=summary"), None, None)
            .await;
        assert_eq!(queried.status, 200, "{path} query");

        let trailing = harness
            .request("GET", &format!("{path}/"), None, None)
            .await;
        assert_eq!(trailing.status, 404, "{path} trailing slash");
        assert_eq!(trailing.json()["error"]["code"], "route_not_found");
    }
}

#[tokio::test]
pub(super) async fn event_stream_requires_the_management_bearer_token() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    let missing = harness
        .request_with("GET", "/api/v1/events/stream", None, None, None, None)
        .await;
    assert_eq!(missing.status, 401);
    assert_eq!(missing.json()["error"]["code"], "unauthorized");
    assert_eq!(missing.header("www-authenticate"), Some("Bearer"));

    let wrong = harness
        .request_with(
            "GET",
            "/api/v1/events/stream",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(wrong.status, 401);
}

#[tokio::test]
pub(super) async fn version_two_event_routes_require_the_management_bearer_token() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;

    for path in ["/api/v2/events", "/api/v2/events/stream"] {
        let missing = harness
            .request_with("GET", path, None, None, None, None)
            .await;
        assert_eq!(missing.status, 401);
        assert_eq!(missing.json()["error"]["code"], "unauthorized");
        assert_eq!(missing.header("www-authenticate"), Some("Bearer"));
    }
}

#[tokio::test]
pub(super) async fn real_tcp_certificate_activation_preserves_v1_and_corrects_v2_event_names() {
    // The UI process fixture has no external ACME CA. This uses the production certificate emitter
    // with the real TCP management service instead of adding a test-only runtime hook.
    let harness = ManagementHarness::start_event_api(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let certificate = "event-boundary-edge";

    let (mut cursor_stream, _) = harness
        .open_versioned_event_stream("/api/v1/events/stream")
        .await;
    let baseline = read_ready_cursor(&mut cursor_stream).await;
    drop(cursor_stream);
    emit_certificate("certificate_activated", "activated", certificate);

    let v1_path = format!("/api/v1/events?after={baseline}&limit=100");
    let v2_path = format!("/api/v2/events?after={baseline}&limit=100");
    let v1_page = harness.request("GET", &v1_path, None, None).await;
    let v2_page = harness.request("GET", &v2_path, None, None).await;
    assert_eq!(v1_page.status, 200);
    assert_eq!(v2_page.status, 200);
    let v1_page = v1_page.json();
    let v2_page = v2_page.json();
    let v1_event = certificate_event(&v1_page, certificate);
    let v2_event = certificate_event(&v2_page, certificate);
    assert_eq!(v1_event["event"], "certificate_activation");
    assert_eq!(v2_event["event"], "certificate_activated");
    assert_eq!(v1_event["outcome"], "activated");
    assert_eq!(v2_event["outcome"], "activated");
    let event_cursor = v2_event["cursor"]
        .as_u64()
        .expect("certificate event cursor");
    assert_eq!(v1_event["cursor"], event_cursor);
    let replay_after = event_cursor.saturating_sub(1);

    let v1_stream_path = format!("/api/v1/events/stream?after={replay_after}");
    let (mut v1_stream, _) = harness.open_versioned_event_stream(&v1_stream_path).await;
    assert_eq!(read_ready_cursor(&mut v1_stream).await, replay_after);
    let v1_frame = String::from_utf8(read_chunk(&mut v1_stream).await).expect("v1 event frame");
    assert!(v1_frame.contains("event: certificate_activated"));
    assert!(v1_frame.contains(r#""event":"certificate_activation""#));
    assert!(v1_frame.contains(r#""certificate":"event-boundary-edge""#));

    let v2_stream_path = format!("/api/v2/events/stream?after={replay_after}");
    let (mut v2_stream, _) = harness.open_versioned_event_stream(&v2_stream_path).await;
    assert_eq!(read_ready_cursor(&mut v2_stream).await, replay_after);
    let v2_frame = String::from_utf8(read_chunk(&mut v2_stream).await).expect("v2 event frame");
    assert!(v2_frame.contains("event: certificate_activated"));
    assert!(v2_frame.contains(r#""event":"certificate_activated""#));
    assert!(v2_frame.contains(r#""certificate":"event-boundary-edge""#));
}

#[tokio::test]
pub(super) async fn event_stream_sends_an_initial_cursor_without_replaying_old_events() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let (mut stream, head) = harness.open_event_stream(None).await;

    let head = String::from_utf8(head).expect("SSE response headers");
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/event-stream")
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
    );
    assert!(head.to_ascii_lowercase().contains("x-correlation-id: op-"));

    let frame = read_chunk(&mut stream).await;
    let frame = String::from_utf8(frame).expect("initial SSE frame");
    assert!(frame.starts_with("event: ready\ndata: {\"cursor\":"));
    assert!(frame.ends_with("}\n\n"));

    drop(stream);
}

#[tokio::test]
pub(super) async fn event_stream_reconnects_from_last_event_id() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let (mut stream, _) = harness.open_event_stream(Some(42)).await;

    let frame = String::from_utf8(read_chunk(&mut stream).await).expect("reconnect SSE frame");
    assert_eq!(frame, "event: ready\ndata: {\"cursor\":42}\n\n");

    drop(stream);
}

#[tokio::test]
pub(super) async fn event_stream_client_cancellation_does_not_block_following_requests() {
    let harness = ManagementHarness::start(
        &(editable_config())
            .validate()
            .expect("valid management fixture"),
    )
    .await;
    let (mut stream, _) = harness.open_event_stream(None).await;
    let _ = read_chunk(&mut stream).await;
    drop(stream);
    tokio::task::yield_now().await;

    assert_eq!(
        harness
            .request("GET", "/api/v1/config", None, None)
            .await
            .status,
        200
    );
}
