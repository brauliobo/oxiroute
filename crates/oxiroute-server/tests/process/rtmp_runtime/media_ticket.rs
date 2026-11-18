use super::*;
use oxiroute_config::{RtmpHlsFragmentNaming, RtmpHlsPolicy};
use std::process::{Child, Command, Stdio};

const FFMPEG: &str = "/usr/bin/ffmpeg";
const FFPROBE: &str = "/usr/bin/ffprobe";
const STREAM: &str = "cam1";
const AVC_CONFIGURATION: [u8; 22] = [
    0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2, 0x68, 0xce,
];

enum Publisher {
    Ffmpeg(Child),
    Wire { _client: Box<RtmpWireClient> },
}

impl Publisher {
    async fn start(rtmp_address: SocketAddr) -> Self {
        if Path::new(FFMPEG).exists() {
            let url = format!("rtmp://{rtmp_address}/live/{STREAM}");
            let child = Command::new(FFMPEG)
                .args(["-hide_banner", "-loglevel", "error", "-re"])
                .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25"])
                .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100"])
                .args(["-t", "60", "-c:v", "libx264", "-preset", "ultrafast"])
                .args(["-profile:v", "baseline", "-pix_fmt", "yuv420p", "-g", "25"])
                .args(["-c:a", "aac", "-ar", "44100", "-ac", "1", "-f", "flv", &url])
                .stdin(Stdio::null())
                .spawn()
                .expect("spawn ffmpeg publisher");
            return Self::Ffmpeg(child);
        }
        let mut client = RtmpWireClient::connect(rtmp_address, "live").await;
        client.publish(STREAM).await;
        client.publish_audio(0, &[0xaf, 0, 0x12, 0x10]).await;
        client.publish_video(0, &AVC_CONFIGURATION).await;
        client
            .publish_video(0, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 1])
            .await;
        client.publish_audio(0, &[0xaf, 1, 2, 3, 4]).await;
        client
            .publish_video(1_000, &[0x27, 1, 0, 0, 0, 0, 0, 0, 2, 0x41, 2])
            .await;
        client
            .publish_video(2_000, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 3])
            .await;
        client
            .publish_video(3_000, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 4])
            .await;
        Self::Wire {
            _client: Box::new(client),
        }
    }

    fn is_ffmpeg(&self) -> bool {
        matches!(self, Self::Ffmpeg(_))
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        if let Self::Ffmpeg(child) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn hls_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    root: &Path,
) -> ConfigDraft {
    let mut config = idle_runtime_config(management_address, rtmp_address);
    config.rtmp_services[0].applications[0].hls = Some(RtmpHlsPolicy {
        root_directory: root.to_path_buf(),
        segment_duration_ms: 1_000,
        max_segment_duration_ms: 4_000,
        playlist_length_ms: 6_000,
        fragment_naming: RtmpHlsFragmentNaming::Sequential,
        nested: false,
        cleanup: true,
        variants: Vec::new(),
        keys: None,
        max_segment_bytes: 4 * 1024 * 1024,
        max_queue_messages: 256,
        max_storage_bytes: 64 * 1024 * 1024,
        max_storage_files: 256,
        max_active_streams: 2,
    });
    config
}

async fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    authorized: bool,
    body: &[u8],
) -> http_support::HttpResponse {
    let authorization = format!("Bearer {TOKEN}");
    let headers = if authorized {
        vec![("Authorization", authorization.as_str())]
    } else {
        Vec::new()
    };
    http_request(address, method, path, &headers, body).await
}

async fn wait_for_ok(address: SocketAddr, path: &str) -> http_support::HttpResponse {
    timeout(Duration::from_secs(40), async {
        loop {
            let response = request(address, "GET", path, false, &[]).await;
            if response.status == 200 {
                return response;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("ticketed playlist publication timed out")
}

async fn probe_streams(url: &str) -> Value {
    let url = url.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(FFPROBE)
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .args(["-analyzeduration", "3000000", "-probesize", "2000000"])
            .args(["-rw_timeout", "30000000"])
            .arg(url)
            .stdin(Stdio::null())
            .output()
    })
    .await
    .expect("ffprobe worker")
    .expect("run ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("ffprobe JSON")
}

#[tokio::test]
async fn a_media_ticket_lets_a_player_fetch_the_playlist_and_segments_without_authorization() {
    let media_root = TempDir::new().expect("HLS root");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = hls_config(management_address, rtmp_address, media_root.path());
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let publisher = Publisher::start(rtmp_address).await;
    wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "live").is_some()
    })
    .await;

    let mint_path = format!("/api/v1/rtmp/media/timer-rtmp/live/{STREAM}/ticket");
    let minted = request(management_address, "POST", &mint_path, true, b"").await;
    assert_eq!(minted.status, 200);
    let body = minted.json();
    let ticket = body["ticket"].as_str().expect("ticket").to_owned();
    let path = body["path"].as_str().expect("ticket path").to_owned();
    let mac = ticket.split_once('.').expect("ticket shape").1.to_owned();
    assert_eq!(
        path,
        format!("/api/v1/rtmp/media/timer-rtmp/live/{STREAM}/index.m3u8?ticket={ticket}")
    );

    let playlist = wait_for_ok(management_address, &path).await;
    let text = String::from_utf8(playlist.body.clone()).expect("playlist text");
    let references = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert!(!references.is_empty(), "{text}");
    let suffix = format!("?ticket={ticket}");
    assert!(references.iter().all(|line| line.ends_with(&suffix)));
    let segment_path = format!(
        "/api/v1/rtmp/media/timer-rtmp/live/{STREAM}/{}",
        references[0]
    );
    let segment = request(management_address, "GET", &segment_path, false, &[]).await;
    assert_eq!(segment.status, 200);
    assert_eq!(segment.body()[0], 0x47);
    assert_eq!(segment.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(segment.header("cache-control"), Some("no-store"));

    if publisher.is_ffmpeg() && Path::new(FFPROBE).exists() {
        let probed = probe_streams(&format!("http://{management_address}{path}")).await;
        let codecs = probed["streams"]
            .as_array()
            .expect("probed streams")
            .iter()
            .filter_map(|stream| stream["codec_name"].as_str())
            .collect::<Vec<_>>();
        assert!(codecs.contains(&"h264"), "{codecs:?}");
    }

    let tampered =
        format!("/api/v1/rtmp/media/timer-rtmp/live/{STREAM}/index.m3u8?ticket={ticket}A");
    let other_stream =
        format!("/api/v1/rtmp/media/timer-rtmp/live/other/index.m3u8?ticket={ticket}");
    for refused in [
        tampered,
        other_stream,
        format!("/api/v1/config?ticket={ticket}"),
        format!("/api/v1/rtmp/streams?ticket={ticket}"),
        format!("/api/v1/rtmp/media/timer-rtmp/live/{STREAM}/index.m3u8?access_token={TOKEN}"),
    ] {
        let response = request(management_address, "GET", &refused, false, &[]).await;
        assert_eq!(response.status, 401, "{refused}");
    }

    let audit = request(
        management_address,
        "GET",
        "/api/v1/audit?limit=1000",
        true,
        &[],
    )
    .await;
    assert_eq!(audit.status, 200);
    let events = request(
        management_address,
        "GET",
        "/api/v1/events?limit=1000",
        true,
        &[],
    )
    .await;
    assert_eq!(events.status, 200);
    assert!(
        audit.text().contains("media_ticket_mint"),
        "{}",
        audit.text()
    );
    for page in [audit.text(), events.text(), server.stderr()] {
        assert!(!page.contains(&mac));
        assert!(!page.contains("ticket="));
    }

    drop(publisher);
    server.shutdown();
}
