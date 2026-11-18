use super::*;
use http_support::HttpResponse;

async fn events(address: SocketAddr) -> Vec<Value> {
    let authorization = format!("Bearer {TOKEN}");
    let response: HttpResponse = http_request(
        address,
        "GET",
        "/api/v2/events?after=0&limit=500",
        &[("Authorization", authorization.as_str())],
        &[],
    )
    .await;
    assert_eq!(response.status, 200);
    response.json()["events"]
        .as_array()
        .cloned()
        .expect("events")
}

pub(super) async fn wait_for_event(address: SocketAddr, name: &str, reason: Option<&str>) -> Value {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let found = events(address).await.into_iter().find(|event| {
                event["event"] == name
                    && reason.is_none_or(|reason| event["rtmp"]["reason"] == reason)
            });
            if let Some(event) = found {
                return event;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("event {name} did not arrive"))
}

fn ffmpeg_is_installed() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn publish_with_ffmpeg(rtmp_address: SocketAddr, stream: &str, seconds: u32) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-re"])
        .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440"])
        .args(["-t", &seconds.to_string()])
        .args([
            "-threads",
            "1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "25",
        ])
        .args(["-c:a", "aac", "-f", "flv"])
        .arg(format!("rtmp://{rtmp_address}/continuous/{stream}"))
        .status()
        .is_ok_and(|status| status.success())
}

fn events_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    root: &Path,
    unused_root: &Path,
) -> ConfigDraft {
    let mut config = runtime_config(management_address, rtmp_address, root, unused_root);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = None;
    config
}

#[tokio::test]
async fn a_normal_ffmpeg_publish_ends_as_closed_with_stream_session_and_recorder_events() {
    if !ffmpeg_is_installed() {
        eprintln!("ffmpeg is not installed; skipping the synthetic publish");
        return;
    }
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "events-recordings");
    let unused = create_secure_root(directory.path(), "events-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = events_config(management_address, rtmp_address, &root, &unused);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    assert!(publish_with_ffmpeg(rtmp_address, "cam", 2));

    let ended = wait_for_event(management_address, "rtmp_publish_end", None).await;
    assert_eq!(ended["outcome"], "applied");
    assert_eq!(ended["rtmp"]["reason"], "closed");
    assert_eq!(ended["rtmp"]["stream"], "cam");
    assert_eq!(ended["rtmp"]["application"], "continuous");
    assert_eq!(ended["rtmp"]["role"], "publisher");
    assert!(ended["rtmp"]["sessionId"].is_string());
    assert!(ended["rtmp"]["streamId"].is_string());
    let started = wait_for_event(management_address, "rtmp_recorder_started", None).await;
    assert_eq!(started["rtmp"]["stream"], "cam");
    assert_eq!(started["rtmp"]["recorder"], "archive");
    wait_for_event(management_address, "rtmp_recorder_stopped", None).await;

    let all = events(management_address).await;
    let publish_failures = all
        .iter()
        .filter(|event| event["event"] == "rtmp_publish" && event["outcome"] == "failed")
        .count();
    assert_eq!(
        publish_failures, 0,
        "a normal end must not be logged as failed"
    );
    let publish = all
        .iter()
        .find(|event| event["event"] == "rtmp_publish")
        .expect("publish event");
    assert_eq!(publish["rtmp"]["stream"], "cam");
    assert!(publish["rtmp"]["sessionId"].is_string());
    server.shutdown();
}

#[tokio::test]
async fn an_operator_drop_ends_the_publisher_with_reason_dropped() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "events-drop-recordings");
    let unused = create_secure_root(directory.path(), "events-drop-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = events_config(management_address, rtmp_address, &root, &unused);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("dock").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12, 0x10]).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| stream["state"] == "live")
    })
    .await;

    let authorization = format!("Bearer {TOKEN}");
    let clients = http_request(
        management_address,
        "GET",
        "/api/v1/rtmp/stats/clients",
        &[("Authorization", authorization.as_str())],
        &[],
    )
    .await;
    let client = clients.json()["clients"][0].clone();
    let path = format!(
        "/api/v1/rtmp/clients/{}/drop",
        client["id"].as_str().expect("id")
    );
    let revision = client["revision"].as_str().expect("revision").to_owned();
    let dropped = http_request(
        management_address,
        "POST",
        &path,
        &[
            ("Authorization", authorization.as_str()),
            ("If-Rtmp-Session-Revision", revision.as_str()),
        ],
        &[],
    )
    .await;
    assert_eq!(dropped.status, 202);

    let ended = wait_for_event(management_address, "rtmp_publish_end", Some("dropped")).await;
    assert_eq!(ended["outcome"], "applied");
    assert_eq!(ended["rtmp"]["stream"], "dock");
    drop(publisher);
    server.shutdown();
}
