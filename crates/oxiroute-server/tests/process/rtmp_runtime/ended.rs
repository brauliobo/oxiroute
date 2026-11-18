use super::*;
use http_support::HttpResponse;

const AAC_STEREO_44100: [u8; 4] = [0xaf, 0x00, 0x12, 0x10];

async fn management(
    address: SocketAddr,
    method: &str,
    path: &str,
    revision: Option<&str>,
) -> HttpResponse {
    let authorization = format!("Bearer {TOKEN}");
    let mut headers = vec![("Authorization", authorization.as_str())];
    if let Some(revision) = revision {
        headers.push(("If-Rtmp-Session-Revision", revision));
    }
    http_request(address, method, path, &headers, &[]).await
}

fn ended_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    root: &Path,
    unused_root: &Path,
) -> ConfigDraft {
    let mut config = runtime_config(management_address, rtmp_address, root, unused_root);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = None;
    config
}

async fn publish_audio_frames(publisher: &mut RtmpWireClient, stream: &str) {
    publisher.publish(stream).await;
    publisher.publish_audio(0, &AAC_STEREO_44100).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
}

fn ended_stream<'a>(catalog: &'a Value, name: &str) -> Option<&'a Value> {
    catalog["recently_ended_streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["name"] == name)
}

#[tokio::test]
async fn a_closed_publisher_stays_listed_offline_and_the_stream_id_survives_a_restart() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "ended-recordings");
    let unused = create_secure_root(directory.path(), "ended-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = ended_config(management_address, rtmp_address, &root, &unused);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publish_audio_frames(&mut publisher, "lobby").await;
    let live = wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["state"] == "live"
                && stream["media"]["profile"]["audio_sample_rate_hz"] == 44_100
        })
    })
    .await;
    let live_stream = stream_for(&live, "continuous").expect("live stream");
    let stream_id = live_stream["id"].as_str().expect("stream id").to_owned();
    assert_eq!(
        live_stream["media"]["profile"]["audio_sample_rate_hz"],
        44_100
    );
    assert_eq!(live_stream["media"]["profile"]["audio_channels"], 2);

    publisher.close().await;
    let offline = wait_for_catalog(management_address, |catalog| {
        ended_stream(catalog, "lobby").is_some()
    })
    .await;
    let ended = ended_stream(&offline, "lobby").expect("ended stream");
    assert_eq!(ended["id"], stream_id.as_str());
    assert_eq!(ended["state"], "offline");
    assert_eq!(ended["end_reason"], "closed");
    assert!(ended["last_seen_at_unix_ms"].as_u64() <= ended["ended_at_unix_ms"].as_u64());
    assert_eq!(ended["media"]["profile"]["audio_sample_rate_hz"], 44_100);

    server.shutdown();
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut again = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publish_audio_frames(&mut again, "lobby").await;
    let restarted = wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| stream["state"] == "live")
    })
    .await;
    assert_eq!(
        stream_for(&restarted, "continuous").expect("stream")["id"],
        stream_id.as_str()
    );
    assert!(ended_stream(&restarted, "lobby").is_none());
    again.close().await;
    server.shutdown();
}

#[tokio::test]
async fn an_operator_drop_is_reported_as_dropped_not_failed() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "dropped-recordings");
    let unused = create_secure_root(directory.path(), "dropped-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = ended_config(management_address, rtmp_address, &root, &unused);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publish_audio_frames(&mut publisher, "dock").await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| stream["state"] == "live")
    })
    .await;

    let clients = management(
        management_address,
        "GET",
        "/api/v1/rtmp/stats/clients",
        None,
    )
    .await;
    let client = &clients.json()["clients"][0];
    let path = format!(
        "/api/v1/rtmp/clients/{}/drop",
        client["id"].as_str().expect("id")
    );
    let revision = client["revision"].as_str().expect("revision").to_owned();
    let dropped = management(management_address, "POST", &path, Some(&revision)).await;
    assert_eq!(dropped.status, 202);

    let offline = wait_for_catalog(management_address, |catalog| {
        ended_stream(catalog, "dock").is_some()
    })
    .await;
    assert_eq!(
        ended_stream(&offline, "dock").expect("ended stream")["end_reason"],
        "dropped"
    );
    drop(publisher);
    server.shutdown();
}
