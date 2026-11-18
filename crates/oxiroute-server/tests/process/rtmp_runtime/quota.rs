use super::*;

fn quota_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    root: &Path,
    unused_root: &Path,
    files: u64,
    retention: oxiroute_config::RtmpRecorderRetention,
) -> ConfigDraft {
    let mut config = runtime_config(management_address, rtmp_address, root, unused_root);
    let recorder = &mut config.rtmp_services[0].applications[0].recorders[0];
    recorder.max_storage_files = Some(files);
    recorder.max_frames = Some(2);
    recorder.retention = retention;
    config
}

async fn publish_frames(publisher: &mut RtmpWireClient, first: u32, count: u32) {
    for timestamp in first..first + count {
        publisher
            .publish_audio(timestamp, &[0xaf, 0x01, 0x40])
            .await;
    }
}

fn recorder_phase(catalog: &Value) -> Option<&Value> {
    stream_for(catalog, "continuous").map(|stream| &stream["recorders"][0]["phase"])
}

fn recording_files(root: &Path) -> Vec<String> {
    let mut names = fs::read_dir(root)
        .expect("recording root")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| !name.starts_with('.'))
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[tokio::test]
async fn file_quota_without_retention_reports_quota_files_and_recovers_when_a_file_is_removed() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "quota-recordings");
    let unused = create_secure_root(directory.path(), "quota-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = quota_config(
        management_address,
        rtmp_address,
        &root,
        &unused,
        1,
        oxiroute_config::RtmpRecorderRetention::Reject,
    );
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publish_frames(&mut publisher, 1, 8).await;

    let failed = wait_for_catalog(management_address, |catalog| {
        recorder_phase(catalog).is_some_and(|phase| phase["state"] == "failed")
    })
    .await;
    assert_eq!(
        recorder_phase(&failed).expect("phase")["code"],
        "quota_files"
    );
    let completed = recording_files(&root);
    assert_eq!(completed.len(), 1);

    fs::remove_file(root.join(&completed[0])).expect("free the only recording");
    let mut next = 20;
    let recovered = timeout(WIRE_TIMEOUT, async {
        loop {
            publish_frames(&mut publisher, next, 2).await;
            next += 2;
            let catalog = wait_for_catalog(management_address, |_| true).await;
            if recorder_phase(&catalog).is_some_and(|phase| phase["state"] == "recording") {
                return catalog;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recorder recovers once space is freed");
    assert_eq!(
        recorder_phase(&recovered).expect("phase")["state"],
        "recording"
    );
    publisher.close().await;
    server.shutdown();
}

#[tokio::test]
async fn delete_oldest_retention_keeps_recording_without_failing_at_the_file_quota() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "retention-recordings");
    let unused = create_secure_root(directory.path(), "retention-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = quota_config(
        management_address,
        rtmp_address,
        &root,
        &unused,
        2,
        oxiroute_config::RtmpRecorderRetention::DeleteOldest,
    );
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publish_frames(&mut publisher, 1, 24).await;

    let catalog = wait_for_catalog(management_address, |catalog| {
        recorder_phase(catalog).is_some_and(|phase| phase["state"] == "recording")
            && stream_for(catalog, "continuous").is_some_and(|stream| {
                stream["recorders"][0]["segments_completed"]
                    .as_str()
                    .and_then(|count| count.parse::<u64>().ok())
                    .is_some_and(|count| count >= 5)
            })
    })
    .await;
    assert_eq!(
        recorder_phase(&catalog).expect("phase")["state"],
        "recording"
    );
    assert!(recording_files(&root).len() <= 2);
    publisher.close().await;
    server.shutdown();
}
