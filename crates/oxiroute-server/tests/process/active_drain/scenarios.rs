#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn http_reload_closes_the_idle_keepalive_and_drain_rejects_new_admissions() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let initial = http_config(management_address, listener_address, "old-http");
    let mut server = ServerProcess::start(&initial.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let mut old_connection = TcpStream::connect(listener_address)
        .await
        .expect("old HTTP connection");
    assert_eq!(
        persistent_request(&mut old_connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"old-http"
    );
    let original_revision = active_revision(management_address, &authorization).await;

    let mut candidate = initial.clone();
    fixed_response_body(&mut candidate, "new-http");
    write_config(&server.config_path, &candidate.clone().validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let mut closed = Vec::new();
    timeout(WIRE_TIMEOUT, old_connection.read_to_end(&mut closed))
        .await
        .expect("the idle HTTP connection stayed open after the reload")
        .expect("read the closed HTTP connection");
    assert!(
        closed.is_empty(),
        "the idle connection was answered after retirement"
    );
    drop(old_connection);
    let mut new_connection = TcpStream::connect(listener_address)
        .await
        .expect("new HTTP connection");
    assert_eq!(
        persistent_request(&mut new_connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"new-http"
    );
    drop(new_connection);

    let revision = active_revision(management_address, &authorization).await;
    let drain = generation_drain(management_address, &authorization, &revision).await;
    assert_eq!(drain.status, 202, "{}", drain.text());
    assert_eq!(drain.json()["outcome"], "draining");
    assert_no_http_response(listener_address).await;
    server.shutdown();
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
pub(super) async fn generation_drain_does_not_leave_process_admission_draining_for_replacement() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let mut initial = http_config(management_address, listener_address, "initial");
    let mut server = ServerProcess::start(&initial.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let revision = active_revision(management_address, &authorization).await;
    let drain = generation_drain(management_address, &authorization, &revision).await;
    assert_eq!(drain.status, 202, "{}", drain.text());

    fixed_response_body(&mut initial, "replacement");
    write_config(&server.config_path, &initial.clone().validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &revision).await;

    let mut connection = TcpStream::connect(listener_address)
        .await
        .expect("replacement HTTP connection");
    assert_eq!(
        persistent_request(&mut connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"replacement"
    );
    drop(connection);
    server.shutdown();
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
pub(super) async fn h2_reload_sends_goaway_while_the_candidate_serves_new_connections() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let mut initial = wire_support::proxy_config(
        listener_address,
        reserve_tcp_address(),
        vec![oxiroute_config::AlpnProtocol::H2],
        None,
        HttpVersionPolicy::default(),
    );
    initial.management = Some(management(management_address));
    initial.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "old-h2".into(),
        headers: Vec::new(),
    };
    fixed_response_body(&mut initial, "old-h2");
    let mut server = ServerProcess::start(&initial.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let mut old_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("old H2 connection");
    assert_eq!(
        old_connection
            .get()
            .await
            .expect("old H2 response")
            .as_ref(),
        b"old-h2"
    );
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;

    let mut candidate = initial;
    fixed_response_body(&mut candidate, "new-h2");
    write_config(&server.config_path, &candidate.clone().validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let old_request = timeout(WIRE_TIMEOUT, old_connection.get())
        .await
        .expect("old H2 request remained pending after GOAWAY");
    assert!(
        old_request.is_err(),
        "old H2 connection accepted a stream after generation quiesce"
    );

    let mut new_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("candidate H2 connection");
    assert_eq!(
        new_connection
            .get()
            .await
            .expect("candidate H2 response")
            .as_ref(),
        b"new-h2"
    );
    new_connection.finish().await;
    old_connection.finish().await;

    server.shutdown();
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
pub(super) async fn forward_h2_reload_sends_goaway_while_the_candidate_serves_new_connections() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let (origin_address, origin_task) = start_echo_upstream().await;
    let private_key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut initial = forward_h2_config(
        management_address,
        listener_address,
        origin_address,
        private_key.path(),
    );
    let mut server = ServerProcess::start(&initial.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let mut old_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("old forward H2 connection");
    assert_eq!(
        old_connection
            .connect(origin_address, b"old-forward-h2")
            .await
            .expect("old forward H2 tunnel")
            .as_ref(),
        b"old-forward-h2"
    );
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;

    initial.max_connections = Some(17);
    write_config(&server.config_path, &initial.clone().validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let old_request = timeout(
        WIRE_TIMEOUT,
        old_connection.connect(origin_address, b"old-after-reload"),
    )
    .await
    .expect("old forward H2 request remained pending after GOAWAY");
    assert!(
        old_request.is_err(),
        "old forward H2 connection accepted a stream after generation quiesce"
    );

    let mut new_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("candidate forward H2 connection");
    assert_eq!(
        new_connection
            .connect(origin_address, b"new-forward-h2")
            .await
            .expect("candidate forward H2 tunnel")
            .as_ref(),
        b"new-forward-h2"
    );
    new_connection.finish().await;
    old_connection.finish().await;

    server.shutdown();
    origin_task.abort();
    let _ = origin_task.await;
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
pub(super) async fn tcp_reload_retains_the_old_relay_and_shutdown_cancels_at_the_deadline() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let (upstream_address, upstream_task) = start_echo_upstream().await;
    let mut initial = tcp_config(management_address, listener_address, upstream_address);
    let mut server = ServerProcess::start(&initial.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let mut old_connection = TcpStream::connect(listener_address)
        .await
        .expect("old TCP relay connection");
    assert_echo(&mut old_connection, b"tcp-before-reload").await;
    let original_revision = active_revision(management_address, &authorization).await;

    initial.max_connections = Some(17);
    write_config(&server.config_path, &initial.clone().validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    assert_echo(&mut old_connection, b"tcp-after-reload").await;

    let revision = active_revision(management_address, &authorization).await;
    let mut management_connection = TcpStream::connect(management_address)
        .await
        .expect("persistent management connection");
    let drain = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/generations/drain",
        &authorization,
        &json!({
            "expectedActiveRevision": revision,
            "timeoutMs": 750,
        }),
    )
    .await;
    assert_eq!(drain.status, 202, "{}", drain.text());
    assert_echo(&mut old_connection, b"tcp-after-drain").await;
    assert_no_tcp_echo(listener_address, b"tcp-during-drain").await;

    let shutdown = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/process/shutdown",
        &authorization,
        &json!({ "expectedActiveRevision": revision }),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());
    drop(management_connection);

    let started = Instant::now();
    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("TCP process shutdown exceeded its deadline")
    .expect("TCP process wait task");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "active TCP relay was not retained through the shutdown deadline: {:?}",
        started.elapsed()
    );

    let mut closed = Vec::new();
    timeout(
        Duration::from_secs(1),
        old_connection.read_to_end(&mut closed),
    )
    .await
    .expect("retained TCP relay did not close after shutdown")
    .expect("read retained TCP relay close");
    drop(old_connection);
    upstream_task.abort();
    let _ = upstream_task.await;
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
pub(super) async fn rtmp_reload_and_drain_retain_the_publisher_until_bounded_shutdown() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let mut initial = rtmp_config(management_address, listener_address);
    let mut server = ServerProcess::start(&initial.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let mut publisher = RtmpWireClient::connect(listener_address, "live").await;
    publisher.publish("old-publisher").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    let original_revision = active_revision(management_address, &authorization).await;

    initial.max_connections = Some(19);
    write_config(&server.config_path, &initial.validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;

    let revision = active_revision(management_address, &authorization).await;
    let mut management_connection = TcpStream::connect(management_address)
        .await
        .expect("persistent RTMP management connection");
    let drain = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/generations/drain",
        &authorization,
        &json!({
            "expectedActiveRevision": revision,
            "timeoutMs": 750,
        }),
    )
    .await;
    assert_eq!(drain.status, 202, "{}", drain.text());
    publisher.publish_audio(3, &[0xaf, 0x01, 0x55]).await;
    assert_no_rtmp_handshake(listener_address).await;

    let revision = active_revision_from_persistent(&mut management_connection).await;
    let shutdown = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/process/shutdown",
        &authorization,
        &json!({ "expectedActiveRevision": revision }),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());
    drop(management_connection);

    let started = Instant::now();
    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("RTMP process shutdown exceeded its deadline")
    .expect("RTMP process wait task");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "RTMP publisher was not retained through the shutdown deadline: {:?}",
        started.elapsed()
    );
    drop(publisher);
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
pub(super) async fn event_sse_closes_with_a_bounded_shutdown_frame_and_releases_its_listener() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let config = management_config_only(management_address);
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let (mut events, _) = open_event_stream(management_address, &authorization, None).await;
    let ready = read_chunk(&mut events).await;
    assert!(
        String::from_utf8_lossy(&ready).starts_with("event: ready\ndata: {\"cursor\":"),
        "unexpected SSE ready frame: {}",
        String::from_utf8_lossy(&ready)
    );

    let authorization = format!("Bearer {TOKEN}");
    let revision = active_revision(management_address, &authorization).await;
    let shutdown = http_request(
        management_address,
        "POST",
        "/api/v1/process/shutdown",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        serde_json::to_vec(&json!({ "expectedActiveRevision": revision }))
            .expect("shutdown request JSON")
            .as_slice(),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());

    let shutdown_frame = timeout(WIRE_TIMEOUT, async {
        loop {
            let frame = read_chunk(&mut events).await;
            if frame.starts_with(b"event: shutdown\n") {
                break frame;
            }
        }
    })
    .await
    .expect("SSE shutdown frame exceeded the process deadline");
    assert_eq!(
        shutdown_frame,
        b"event: shutdown\ndata: {\"reason\":\"server_shutdown\"}\n\n"
    );
    let mut tail = Vec::new();
    timeout(Duration::from_secs(30), events.read_to_end(&mut tail))
        .await
        .expect("SSE stream did not close after shutdown frame")
        .expect("read SSE stream close");

    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("SSE process shutdown exceeded its bounded close window")
    .expect("SSE process wait task");
    assert_listener_released([management_address]);
}
