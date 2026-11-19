use super::*;

#[tokio::test]
async fn rtmp_file_access_log_emits_fixed_lifecycle_records_and_flushes_on_shutdown() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "continuous-private-root");
    let manual_root = create_secure_root(recording_directory.path(), "manual-private-root");
    let log_directory = TempDir::new().expect("access log directory");
    let access_log_path = log_directory.path().join("rtmp-access.jsonl");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    config.rtmp_services[0].access_log = Some(AccessLogPolicy::File {
        path: access_log_path.clone(),
    });

    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera?token=wire-access-secret").await;
    let mut viewer = RtmpWireClient::connect(rtmp_address, "continuous").await;
    viewer.play("camera?viewer=wire-viewer-secret").await;
    viewer.close().await;
    publisher.close().await;
    server.shutdown_gracefully();

    let contents = fs::read_to_string(access_log_path).expect("flushed RTMP access log");
    let events = contents
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("RTMP JSONL event"))
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| event["event"] == "connect"));
    assert!(events.iter().any(|event| event["event"] == "publish"));
    assert!(events.iter().any(|event| event["event"] == "play"));
    assert!(events.iter().any(|event| event["event"] == "disconnect"));
    assert!(events.iter().any(|event| event["event"] == "record"));
    for event in &events {
        assert!(matches!(
            event["event"].as_str(),
            Some("connect" | "disconnect" | "publish" | "play" | "record" | "relay")
        ));
        assert!(event["result"].is_string());
        assert_eq!(event["listener"], "wire-rtmp");
        assert_eq!(event["service"], "wire-rtmp");
        assert!(event["sessionId"].is_string());
        assert!(event["bytesReceived"].is_u64());
        assert!(event["bytesSent"].is_u64());
        assert!(event["messagesReceived"].is_u64());
        assert!(event["messagesSent"].is_u64());
        assert!(event["timestampUnixMs"].is_u64());
        assert!(event["durationMs"].is_u64());
        assert!(event["correlationId"].is_string());
        assert!(event.get("clientIp").is_none());
        assert!(event.get("query").is_none());
        assert!(event.get("token").is_none());
        assert!(event.get("payload").is_none());
        assert!(event.get("outcome").is_none());
    }
    assert!(!contents.contains("wire-access-secret"));
    assert!(!contents.contains("wire-viewer-secret"));
}

#[tokio::test]
async fn continuous_recording_resumes_after_process_restart_and_publisher_reconnect() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "continuous-recordings");
    let manual_root = create_secure_root(recording_directory.path(), "unused-manual-recordings");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    let recorder = &mut config.rtmp_services[0].applications[0].recorders[0];
    recorder.suffix_template = "-%Y%m%d_%H%M%S.mp4".into();
    recorder.append_unix_seconds = true;
    recorder.timezone =
        oxiroute_config::RtmpRecorderTimezone::Iana("America/Argentina/Buenos_Aires".into());
    recorder.time_basis = oxiroute_config::RtmpRecorderTimeBasis::SegmentStart;
    recorder.segment_naming = oxiroute_config::RtmpRecorderSegmentNaming::NginxCompatible;
    recorder.rotation_interval_ms = Some(3_600_000);

    let mut first_server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    first_server.wait_for_tcp(management_address).await;
    first_server.wait_for_tcp(rtmp_address).await;
    let mut first_publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    first_publisher.publish("camera").await;
    first_publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    first_publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["recorders"][0]["phase"]["state"] == "recording"
                && stream["recorders"][0]["bytes_written"]
                    .as_str()
                    .and_then(|bytes| bytes.parse::<u64>().ok())
                    .is_some_and(|bytes| bytes > 13)
        })
    })
    .await;

    first_server.shutdown();
    drop(first_publisher);
    let first_files = wait_for_recording_file_count(&continuous_root, 1).await;
    assert_recording_extensions(&first_files, "mp4");
    let first_path = first_files[0].clone();
    let first_bytes = fs::read(&first_path).expect("first continuous segment");
    assert!(first_bytes.starts_with(b"FLV"));

    let mut second_server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    second_server.wait_for_tcp(management_address).await;
    second_server.wait_for_tcp(rtmp_address).await;
    let mut second_publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    second_publisher.publish("camera").await;
    second_publisher.publish_audio(3, &[0xaf, 0x00, 0x12]).await;
    second_publisher.publish_audio(4, &[0xaf, 0x01, 0x55]).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["recorders"][0]["phase"]["state"] == "recording"
                && stream["recorders"][0]["bytes_written"] != "0"
        })
    })
    .await;
    second_publisher.close().await;

    let resumed_files = wait_for_recording_file_count(&continuous_root, 1).await;
    assert_recording_extensions(&resumed_files, "mp4");
    assert_eq!(resumed_files, vec![first_path.clone()]);
    let resumed_bytes = fs::read(first_path).expect("resumed continuous segment");
    assert!(resumed_bytes.starts_with(b"FLV"));
    assert!(resumed_bytes.len() > first_bytes.len());
    second_server.shutdown();
}

#[tokio::test]
async fn process_shutdown_waits_for_stalled_recorder_cleanup_without_exceeding_the_budget() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "stalled-recordings");
    let manual_root = create_secure_root(recording_directory.path(), "unused-manual-recordings");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    // Listeners bind before the recording store opens; only an answer from the management API
    // shows the store is open, and stalling it earlier would stall startup instead.
    active_revision(management_address, &format!("Bearer {TOKEN}")).await;
    let ownership = fs::File::open(&continuous_root).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall recording storage");
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    sleep(Duration::from_millis(50)).await;
    publisher.close().await;
    sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    server.shutdown_gracefully();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(500),
        "process exited before recorder cleanup was bounded: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "recorder cleanup exceeded the process budget: {elapsed:?}"
    );
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");
}

#[tokio::test]
async fn evicted_generation_keeps_recording_live_connections_until_final_shutdown() {
    let recording_directory = TempDir::new().expect("recording directory");
    let continuous_root = create_secure_root(recording_directory.path(), "previous-recordings");
    let manual_root = create_secure_root(recording_directory.path(), "unused-manual-recordings");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(
        management_address,
        rtmp_address,
        &continuous_root,
        &manual_root,
    );
    config.rtmp_services[0].applications[0].recorders[0].shutdown_timeout_ms = 3_000;
    config.rtmp_services[0].applications[1].recorders[0].start = RtmpRecorderStart::Continuous;
    config.rtmp_services[0].applications[1].recorders[0].shutdown_timeout_ms = 30_000;
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;
    let ownership = fs::File::open(&manual_root).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("stall second recorder");
    let mut stalled = RtmpWireClient::connect(rtmp_address, "manual").await;
    stalled.publish("blocked").await;
    stalled.publish_audio(1, &[0xaf, 0x01, 0x33]).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;
    let initial_size = wait_for_recording_growth(&continuous_root, 0).await;

    config.max_connections = Some(64);
    write_config(&server.config_path, &config.clone().validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    publisher.publish_audio(3, &[0xaf, 0x01, 0x55]).await;
    let second_size = wait_for_recording_growth(&continuous_root, initial_size).await;
    let second_revision = active_revision(management_address, &authorization).await;
    config.max_connections = Some(65);
    write_config(&server.config_path, &config.validate().unwrap());
    wait_for_new_revision(management_address, &authorization, &second_revision).await;
    publisher.publish_audio(4, &[0xaf, 0x01, 0x66]).await;
    wait_for_recording_growth(&continuous_root, second_size).await;
    publisher.close().await;
    let files = wait_for_recording_file_count(&continuous_root, 1).await;
    let recording_path = files[0].clone();
    let recording = timeout(WIRE_TIMEOUT, async {
        loop {
            let recording = fs::read(&recording_path).expect("evicted generation recording");
            if recording
                .windows(3)
                .any(|bytes| bytes == [0xaf, 0x01, 0x66])
            {
                return recording;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("evicted generation recording did not drain");
    assert!(
        recording
            .windows(3)
            .any(|bytes| bytes == [0xaf, 0x01, 0x55])
    );
    assert!(
        recording
            .windows(3)
            .any(|bytes| bytes == [0xaf, 0x01, 0x66])
    );
    stalled.close().await;
    sleep(Duration::from_millis(150)).await;

    let shutdown = tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        server.terminate();
        started.elapsed()
    });
    sleep(Duration::from_millis(250)).await;
    assert!(
        !shutdown.is_finished(),
        "process shutdown did not retain evicted recorder cleanup"
    );
    let elapsed = timeout(WIRE_TIMEOUT, shutdown)
        .await
        .expect("process shutdown timeout")
        .expect("process shutdown task");
    flock(&ownership, FlockOperation::Unlock).expect("release recording storage");

    assert!(
        elapsed >= Duration::from_secs(4),
        "process did not apply its deadline to evicted recorder cleanup: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "evicted generation recorder exceeded the process budget: {elapsed:?}"
    );
}
