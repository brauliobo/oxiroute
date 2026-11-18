use std::process::{Command, Stdio};

use super::*;

const SEQUENCE_START: &[u8] = &[0x90, b'h', b'v', b'c', b'1', 0x01, 0x01, 0x60, 0x00, 0x00];
const COLOR_INFO: &[u8] = &[
    0xd4, b'h', b'v', b'c', b'1', 0x02, 0x00, 0x09, b'c', b'o', b'l', b'o', b'r', b'I', b'n', b'f',
    b'o', 0x03, 0x00, 0x00, 0x09,
];
const KEYFRAME: &[u8] = &[
    0x91, b'h', b'v', b'c', b'1', 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x40,
];
const INTERFRAME: &[u8] = &[
    0xa1, b'h', b'v', b'c', b'1', 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02,
];
const INTERFRAME_X: &[u8] = &[0xa3, b'h', b'v', b'c', b'1', 0x00, 0x00, 0x00, 0x01, 0x02];

fn hevc_video(catalog: &Value) -> Option<&Value> {
    stream_for(catalog, "live").map(|stream| &stream["media"]["video"])
}

fn publish_failures(events: &Value) -> Vec<&Value> {
    events["events"]
        .as_array()
        .expect("event page")
        .iter()
        .filter(|event| event["event"] == "rtmp_publish" && event["outcome"] == "failed")
        .collect()
}

async fn event_page(management_address: SocketAddr) -> Value {
    let authorization = format!("Bearer {TOKEN}");
    http_request(
        management_address,
        "GET",
        "/api/v2/events?after=0&limit=1000",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()
}

fn libx265_available() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(" libx265 "))
}

#[tokio::test]
async fn enhanced_hevc_publisher_with_color_info_stays_connected_and_is_cataloged() {
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = idle_runtime_config(management_address, rtmp_address);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let mut publisher = RtmpWireClient::connect(rtmp_address, "live").await;
    publisher.publish("hevc-wire").await;
    let packets: [(&[u8], u32); 6] = [
        (SEQUENCE_START, 0),
        (COLOR_INFO, 0),
        (KEYFRAME, 0),
        (INTERFRAME, 40),
        (INTERFRAME_X, 80),
        (COLOR_INFO, 80),
    ];
    let expected_bytes = packets
        .iter()
        .map(|(payload, _)| payload.len())
        .sum::<usize>()
        .to_string();
    for (payload, timestamp) in packets {
        publisher.publish_video(timestamp, payload).await;
    }

    let catalog = wait_for_catalog(management_address, |catalog| {
        hevc_video(catalog).is_some_and(|video| video["payload_bytes"] == expected_bytes.as_str())
    })
    .await;
    let video = hevc_video(&catalog).expect("video track");
    assert_eq!(video["codec_fourcc"], "hvc1");
    assert_eq!(video["codec_name"], "hevc");
    assert!(stream_for(&catalog, "live").is_some_and(|stream| stream["publisher"].is_object()));
    assert!(publish_failures(&event_page(management_address).await).is_empty());
    publisher.close().await;
}

#[tokio::test]
async fn ffmpeg_enhanced_hevc_publish_stays_connected_and_is_cataloged() {
    if !libx265_available() {
        eprintln!("skipping: ffmpeg with the libx265 encoder is not available");
        return;
    }
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = idle_runtime_config(management_address, rtmp_address);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let ffmpeg = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-re"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25",
            "-t",
            "2",
        ])
        .args(["-c:v", "libx265", "-an", "-f", "flv"])
        .arg(format!("rtmp://{rtmp_address}/live/hevc-ffmpeg"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ffmpeg");

    let catalog = wait_for_catalog(management_address, |catalog| {
        hevc_video(catalog).is_some_and(|video| video["codec_fourcc"] == "hvc1")
    })
    .await;
    assert_eq!(
        hevc_video(&catalog).expect("video track")["codec_name"],
        "hevc"
    );
    let output = tokio::task::spawn_blocking(move || ffmpeg.wait_with_output())
        .await
        .expect("join ffmpeg")
        .expect("wait for ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg publish failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
