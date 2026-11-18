use super::*;
use http_support::HttpResponse;

async fn management(
    address: SocketAddr,
    method: &str,
    path: &str,
    range: Option<&str>,
) -> HttpResponse {
    let authorization = format!("Bearer {TOKEN}");
    let mut headers = vec![("Authorization", authorization.as_str())];
    if let Some(range) = range {
        headers.push(("Range", range));
    }
    http_request(address, method, path, &headers, &[]).await
}

fn inventory_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    root: &Path,
    unused_root: &Path,
) -> ConfigDraft {
    let mut config = runtime_config(management_address, rtmp_address, root, unused_root);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = Some(4);
    config
}

async fn publish_and_close(rtmp_address: SocketAddr, stream: &str, frames: u32) {
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish(stream).await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    for timestamp in 1..=frames {
        publisher
            .publish_audio(timestamp * 20, &[0xaf, 0x01, 0x40])
            .await;
    }
    publisher.close().await;
}

async fn wait_for_recordings(
    address: SocketAddr,
    query: &str,
    predicate: impl Fn(&[Value]) -> bool,
) -> Value {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let response = management(
                address,
                "GET",
                &format!("/api/v1/rtmp/recordings{query}"),
                None,
            )
            .await;
            assert_eq!(response.status, 200);
            let listing = response.json();
            if predicate(listing["recordings"].as_array().expect("recordings")) {
                return listing;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recording inventory condition timed out")
}

#[tokio::test]
async fn recordings_are_listed_downloaded_with_ranges_deleted_and_keep_their_ids_across_restart() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "inventory-recordings");
    let unused = create_secure_root(directory.path(), "inventory-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = inventory_config(management_address, rtmp_address, &root, &unused);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    publish_and_close(rtmp_address, "camera", 12).await;
    let listing = wait_for_recordings(management_address, "?stream=camera", |recordings| {
        recordings.len() >= 3 && recordings.iter().all(|entry| entry["status"] == "complete")
    })
    .await;
    let recordings = listing["recordings"].as_array().expect("recordings");
    let first = &recordings[0];
    assert_eq!(first["stream"], "camera");
    assert_eq!(first["audio_codec"], "aac");
    assert_eq!(first["recorders"][0]["recorder"], "archive");
    assert!(first["started_at_unix_ms"].is_u64());
    assert!(first["duration_ms"].is_u64());
    let name = first["name"].as_str().expect("name");
    let on_disk = fs::read(root.join(name)).expect("recording file");
    assert_eq!(first["size_bytes"], on_disk.len().to_string());
    assert_eq!(
        listing["roots"][0]["files"].as_u64(),
        Some(recordings.len() as u64)
    );
    let id = first["id"].as_str().expect("id").to_owned();

    let metadata = management(
        management_address,
        "GET",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(metadata.status, 200);
    assert_eq!(metadata.json()["id"], id);

    let content_path = format!("/api/v1/rtmp/recordings/{id}/content");
    let full = management(management_address, "GET", &content_path, None).await;
    assert_eq!(full.status, 200);
    assert_eq!(full.body, on_disk);
    assert!(full.headers["content-disposition"].starts_with("attachment; filename=\""));
    assert_eq!(full.headers["accept-ranges"], "bytes");
    let partial = management(management_address, "GET", &content_path, Some("bytes=2-9")).await;
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, on_disk[2..10]);
    assert_eq!(
        partial.headers["content-range"],
        format!("bytes 2-9/{}", on_disk.len())
    );
    let beyond = format!("bytes={}-", on_disk.len());
    let invalid = management(management_address, "GET", &content_path, Some(&beyond)).await;
    assert_eq!(invalid.status, 416);

    server.shutdown();
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let restarted = wait_for_recordings(management_address, "?stream=camera", |recordings| {
        !recordings.is_empty()
    })
    .await;
    assert!(
        restarted["recordings"]
            .as_array()
            .expect("recordings")
            .iter()
            .any(|entry| entry["id"] == id.as_str() && entry["status"] == "complete"),
        "the recording id must survive a restart"
    );

    let deleted = management(
        management_address,
        "DELETE",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(deleted.status, 200);
    assert_eq!(deleted.json()["deleted"]["id"], id);
    assert!(!root.join(name).exists());
    let gone = management(
        management_address,
        "GET",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(gone.status, 404);
    server.shutdown();
}

#[tokio::test]
async fn an_active_recording_is_listed_as_recording_and_cannot_be_deleted() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "active-recordings");
    let unused = create_secure_root(directory.path(), "active-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(management_address, rtmp_address, &root, &unused);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = None;
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("live-camera").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;

    let listing = wait_for_recordings(management_address, "?status=recording", |recordings| {
        recordings.len() == 1
    })
    .await;
    let id = listing["recordings"][0]["id"].as_str().expect("id");
    let refused = management(
        management_address,
        "DELETE",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(refused.status, 409);
    assert_eq!(refused.json()["error"]["code"], "recording_active");
    publisher.close().await;
    server.shutdown();
}

#[tokio::test]
async fn a_recording_cut_short_by_a_crash_is_listed_partial_after_restart() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "crash-recordings");
    let unused = create_secure_root(directory.path(), "crash-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(management_address, rtmp_address, &root, &unused);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = None;
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("crashed").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
    wait_for_recordings(management_address, "?status=recording", |recordings| {
        recordings.len() == 1
    })
    .await;
    let killed = std::process::Command::new("kill")
        .args(["-9", &server.pid().to_string()])
        .status()
        .expect("kill the daemon");
    assert!(killed.success());
    drop(server);
    drop(publisher);

    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let listing = wait_for_recordings(management_address, "?stream=crashed", |recordings| {
        recordings.len() == 1
    })
    .await;
    assert_eq!(listing["recordings"][0]["status"], "partial");
    let name = listing["recordings"][0]["name"]
        .as_str()
        .expect("name")
        .to_owned();

    let mut again = RtmpWireClient::connect(rtmp_address, "continuous").await;
    again.publish("crashed").await;
    again.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    again.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
    let catalog = wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["recorders"][0]["recoverable_partial_name"] == name.as_str()
        })
    })
    .await;
    assert_eq!(
        stream_for(&catalog, "continuous").expect("stream")["recorders"][0]["recoverable_partial_name"],
        name.as_str()
    );
    again.close().await;
    server.shutdown();
}
