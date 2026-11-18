#[allow(clippy::wildcard_imports)]
use super::*;
use oxiroute_config::{RtmpHlsFragmentNaming, RtmpHlsPolicy};
use serde_json::json;

const MEDIA_BASE: &str = "/api/v1/rtmp/media/live/broadcast";
const AVC_CONFIGURATION: [u8; 22] = [
    0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2, 0x68, 0xce,
];

fn hls_policy(root: &TempDir) -> RtmpHlsPolicy {
    RtmpHlsPolicy {
        root_directory: root.path().to_path_buf(),
        segment_duration_ms: 1_000,
        max_segment_duration_ms: 2_000,
        playlist_length_ms: 6_000,
        fragment_naming: RtmpHlsFragmentNaming::Sequential,
        nested: false,
        cleanup: true,
        variants: Vec::new(),
        keys: None,
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 16,
        max_storage_bytes: 4 * 1024 * 1024,
        max_storage_files: 64,
        max_active_streams: 2,
    }
}

fn dash_policy(root: &TempDir) -> RtmpDashPolicy {
    RtmpDashPolicy {
        root_directory: root.path().to_path_buf(),
        segment_duration_ms: 1_000,
        max_segment_duration_ms: 2_000,
        playlist_length_ms: 6_000,
        segment_naming: RtmpDashSegmentNaming::Sequential,
        nested: true,
        cleanup: true,
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 16,
        max_storage_bytes: 4 * 1024 * 1024,
        max_storage_files: 64,
        max_active_streams: 2,
    }
}

fn media_config(hls: Option<RtmpHlsPolicy>, dash: Option<RtmpDashPolicy>) -> ConfigDraft {
    let mut config = candidate_config(&empty_config(), "live");
    let mut plain = config.rtmp_services[0].applications[0].clone();
    plain.name = "plain".into();
    config.rtmp_services[0].applications[0].hls = hls;
    config.rtmp_services[0].applications[0].dash = dash;
    config.rtmp_services[0].applications.push(plain);
    config
}

async fn start(config: ConfigDraft) -> ManagementHarness {
    ManagementHarness::start(&config.validate().expect("valid ticket fixture")).await
}

fn publish_h264(harness: &ManagementHarness, stream: &str) -> RtmpSessionClient {
    let mut publisher = RtmpSessionClient::connect_handle(&harness.rtmp_runtime(), "broadcast");
    publisher.publish(stream, 1_000);
    publisher.publish_audio(0, &[0xaf, 0, 0x12, 0x10], 1_001);
    publisher.publish_video(0, &AVC_CONFIGURATION, 1_002);
    publisher.publish_video(0, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 1], 1_003);
    publisher.publish_audio(0, &[0xaf, 1, 2, 3, 4], 1_004);
    publisher.publish_video(1_000, &[0x27, 1, 0, 0, 0, 0, 0, 0, 2, 0x41, 2], 1_005);
    publisher.publish_video(2_000, &[0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 3], 1_006);
    publisher
}

async fn mint(harness: &ManagementHarness, stream: &str, body: Option<&Value>) -> HttpResponse {
    harness
        .request("POST", &format!("{MEDIA_BASE}/{stream}/ticket"), None, body)
        .await
}

async fn get_with_ticket(harness: &ManagementHarness, path: &str) -> HttpResponse {
    harness
        .request_with("GET", path, None, None, None, None)
        .await
}

async fn wait_for_ticketed(harness: &ManagementHarness, path: &str) -> HttpResponse {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = get_with_ticket(harness, path).await;
        if response.status == 200 {
            return response;
        }
        assert!(Instant::now() < deadline, "media publication timeout");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn body_text(response: &HttpResponse) -> String {
    String::from_utf8(response.body.clone()).expect("UTF-8 body")
}

fn segment_references(playlist: &str) -> Vec<&str> {
    playlist
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

fn flip_last_character(ticket: &str) -> String {
    let mut characters = ticket.chars().collect::<Vec<_>>();
    let last = characters.pop().expect("ticket characters");
    characters.push(if last == 'A' { 'B' } else { 'A' });
    characters.into_iter().collect()
}

#[tokio::test]
pub(super) async fn a_minted_ticket_serves_the_hls_playlist_and_its_segments_without_authorization()
{
    let root = TempDir::new().expect("HLS root");
    let harness = start(media_config(Some(hls_policy(&root)), None)).await;
    let _publisher = publish_h264(&harness, "camera");

    let minted = mint(&harness, "camera", None).await;
    assert_eq!(minted.status, 200);
    assert_eq!(minted.header("cache-control"), Some("no-store"));
    assert_eq!(minted.header("referrer-policy"), Some("no-referrer"));
    let body = minted.json();
    let ticket = body["ticket"].as_str().expect("ticket").to_owned();
    assert_eq!(body["ttlSeconds"], 90);
    assert_eq!(
        body["path"],
        format!("{MEDIA_BASE}/camera/index.m3u8?ticket={ticket}")
    );
    let expires = body["expiresAtUnixMs"].as_u64().expect("expiry");
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("clock in range");
    assert!((now + 80_000..=now + 91_000).contains(&expires));

    let path = body["path"].as_str().expect("path").to_owned();
    let playlist = wait_for_ticketed(&harness, &path).await;
    assert_eq!(
        playlist.header("content-type"),
        Some("application/vnd.apple.mpegurl")
    );
    assert_eq!(playlist.header("cache-control"), Some("no-store"));
    assert_eq!(playlist.header("referrer-policy"), Some("no-referrer"));
    let text = body_text(&playlist);
    let references = segment_references(&text);
    assert!(!references.is_empty(), "{text}");
    let suffix = format!("?ticket={ticket}");
    for reference in &references {
        assert!(reference.ends_with(&suffix), "{reference}");
    }

    let segment =
        get_with_ticket(&harness, &format!("{MEDIA_BASE}/camera/{}", references[0])).await;
    assert_eq!(segment.status, 200);
    assert_eq!(segment.header("content-type"), Some("video/mp2t"));
    assert_eq!(segment.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(segment.body()[0], 0x47);

    let bare = references[0].trim_end_matches(&suffix);
    let without_ticket = get_with_ticket(&harness, &format!("{MEDIA_BASE}/camera/{bare}")).await;
    assert_eq!(without_ticket.status, 401);
    assert_eq!(without_ticket.header("referrer-policy"), None);

    let bearer = harness
        .request(
            "GET",
            &format!("{MEDIA_BASE}/camera/index.m3u8"),
            None,
            None,
        )
        .await;
    assert_eq!(bearer.status, 200);
    assert!(!body_text(&bearer).contains("ticket="));
    assert_eq!(bearer.header("cache-control"), None);
}

#[tokio::test]
pub(super) async fn a_minted_ticket_serves_the_dash_manifest_and_its_segments() {
    let root = TempDir::new().expect("DASH root");
    let harness = start(media_config(None, Some(dash_policy(&root)))).await;
    let _publisher = publish_h264(&harness, "camera");

    let body = mint(&harness, "camera", Some(&json!({ "ttlSeconds": 600 })))
        .await
        .json();
    assert_eq!(body["ttlSeconds"], 600);
    let ticket = body["ticket"].as_str().expect("ticket");
    assert_eq!(
        body["path"],
        format!("{MEDIA_BASE}/camera/manifest.mpd?ticket={ticket}")
    );
    let manifest = wait_for_ticketed(&harness, body["path"].as_str().unwrap()).await;
    let text = body_text(&manifest);
    assert!(
        text.contains(&format!("sourceURL=\"init.mp4?ticket={ticket}\"")),
        "{text}"
    );
    assert!(
        text.contains(&format!("media=\"seg-0.m4s?ticket={ticket}\"")),
        "{text}"
    );

    for object in ["init.mp4", "seg-0.m4s"] {
        let response = get_with_ticket(
            &harness,
            &format!("{MEDIA_BASE}/camera/{object}?ticket={ticket}"),
        )
        .await;
        assert_eq!(response.status, 200, "{object}");
    }
}

#[tokio::test]
pub(super) async fn a_ticket_is_bound_to_the_stream_it_was_minted_for() {
    let root = TempDir::new().expect("HLS root");
    let harness = start(media_config(Some(hls_policy(&root)), None)).await;
    let _publisher = publish_h264(&harness, "camera");
    let ticket = mint(&harness, "camera", None).await.json()["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();

    for path in [
        format!("{MEDIA_BASE}/doorbell/index.m3u8?ticket={ticket}"),
        format!("{MEDIA_BASE}/nothing/index.m3u8?ticket={ticket}"),
        format!("/api/v1/rtmp/media/live/plain/camera/index.m3u8?ticket={ticket}"),
        format!("/api/v1/rtmp/media/other/broadcast/camera/index.m3u8?ticket={ticket}"),
    ] {
        let response = get_with_ticket(&harness, &path).await;
        assert_eq!(response.status, 401, "{path}");
        assert_eq!(response.json()["error"]["code"], "invalid_ticket", "{path}");
    }
}

#[tokio::test]
pub(super) async fn tampered_malformed_and_repeated_tickets_are_refused() {
    let root = TempDir::new().expect("HLS root");
    let harness = start(media_config(Some(hls_policy(&root)), None)).await;
    let ticket = mint(&harness, "camera", None).await.json()["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();
    let (expiry, mac) = ticket.split_once('.').expect("ticket shape");
    let later = expiry.parse::<u64>().expect("expiry") + 60_000;
    let index = format!("{MEDIA_BASE}/camera/index.m3u8");

    for query in [
        format!("ticket={}", flip_last_character(&ticket)),
        format!("ticket={later}.{mac}"),
        format!("ticket={expiry}"),
        "ticket=".to_owned(),
        "ticket=.".to_owned(),
        format!("ticket={ticket}&ticket={ticket}"),
        format!("ticket={}", ticket.to_uppercase()),
        format!("access_token={TEST_TOKEN}"),
        format!("ticket={TEST_TOKEN}"),
    ] {
        let response = get_with_ticket(&harness, &format!("{index}?{query}")).await;
        assert_eq!(response.status, 401, "{query}");
    }
}

#[tokio::test]
pub(super) async fn a_ticket_is_not_an_authorization_for_any_other_route_or_method() {
    let root = TempDir::new().expect("HLS root");
    let harness = start(media_config(Some(hls_policy(&root)), None)).await;
    let ticket = mint(&harness, "camera", None).await.json()["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();

    for path in [
        "/api/v1/config",
        "/api/v1/status",
        "/api/v1/listeners",
        "/api/v1/audit",
        "/api/v1/events",
        "/api/v1/rtmp/streams",
        "/api/v1/rtmp/stats",
    ] {
        let response = get_with_ticket(&harness, &format!("{path}?ticket={ticket}")).await;
        assert_eq!(response.status, 401, "GET {path}");
    }
    let mint_with_ticket = harness
        .request_with(
            "POST",
            &format!("{MEDIA_BASE}/camera/ticket?ticket={ticket}"),
            None,
            None,
            None,
            None,
        )
        .await;
    assert_eq!(mint_with_ticket.status, 401);
    for method in ["POST", "PUT", "DELETE"] {
        let response = harness
            .request_with(
                method,
                &format!("{MEDIA_BASE}/camera/index.m3u8?ticket={ticket}"),
                None,
                None,
                None,
                None,
            )
            .await;
        assert_eq!(response.status, 401, "{method}");
    }
}

#[tokio::test]
pub(super) async fn minting_validates_the_media_output_the_lifetime_and_the_credentials() {
    let root = TempDir::new().expect("HLS root");
    let harness = start(media_config(Some(hls_policy(&root)), None)).await;

    let unauthenticated = harness
        .request_with(
            "POST",
            &format!("{MEDIA_BASE}/camera/ticket"),
            None,
            None,
            None,
            None,
        )
        .await;
    assert_eq!(unauthenticated.status, 401);

    for path in [
        "/api/v1/rtmp/media/live/plain/camera/ticket",
        "/api/v1/rtmp/media/live/missing/camera/ticket",
        "/api/v1/rtmp/media/nothing/broadcast/camera/ticket",
    ] {
        let response = harness.request("POST", path, None, None).await;
        assert_eq!(response.status, 404, "{path}");
    }

    for body in [
        json!({ "ttlSeconds": 0 }),
        json!({ "ttlSeconds": 601 }),
        json!({ "ttlSeconds": -1 }),
        json!({ "ttlSeconds": "90" }),
        json!({ "ttl": 90 }),
    ] {
        let response = mint(&harness, "camera", Some(&body)).await;
        assert_eq!(response.status, 400, "{body}");
    }
    for ttl in [1, 600] {
        let response = mint(&harness, "camera", Some(&json!({ "ttlSeconds": ttl }))).await;
        assert_eq!(response.status, 200);
        assert_eq!(response.json()["ttlSeconds"], ttl);
    }

    let wrong_method = harness
        .request("PUT", &format!("{MEDIA_BASE}/camera/ticket"), None, None)
        .await;
    assert_eq!(wrong_method.status, 405);
    assert_eq!(wrong_method.header("allow"), Some("POST"));
}

#[tokio::test]
pub(super) async fn a_ticket_serves_one_vod_object_without_authorization() {
    let directory = TempDir::new().expect("VOD directory");
    fs::write(directory.path().join("movie.flv"), b"0123456789").expect("VOD object");
    fs::write(directory.path().join("other.flv"), b"abcdefghij").expect("VOD object");
    let mut config = candidate_config(&empty_config(), "live");
    config.rtmp_services[0].applications[0].vod = Some(RtmpVodPolicy {
        sources: vec![RtmpVodSource::Local {
            name: "archive".into(),
            root_directory: directory.path().to_path_buf(),
        }],
        max_sessions: 4,
        max_file_bytes: 1_024,
        max_duration_ms: 60_000,
    });
    let harness = start(config).await;
    let mint_path = "/api/v1/rtmp/vod/live/broadcast/archive/ticket";

    let minted = harness
        .request(
            "POST",
            mint_path,
            None,
            Some(&json!({ "path": "movie.flv", "ttlSeconds": 30 })),
        )
        .await;
    assert_eq!(minted.status, 200);
    assert_eq!(minted.header("referrer-policy"), Some("no-referrer"));
    let body = minted.json();
    let ticket = body["ticket"].as_str().expect("ticket").to_owned();
    assert_eq!(body["ttlSeconds"], 30);
    let path = body["path"].as_str().expect("path");
    assert_eq!(
        path,
        format!("/api/v1/rtmp/vod/live/broadcast/archive/movie.flv?ticket={ticket}")
    );

    let object = get_with_ticket(&harness, path).await;
    assert_eq!(object.status, 200);
    assert_eq!(object.body(), b"0123456789");
    assert_eq!(object.header("cache-control"), Some("no-store"));
    assert_eq!(object.header("referrer-policy"), Some("no-referrer"));

    let other = get_with_ticket(
        &harness,
        &format!("/api/v1/rtmp/vod/live/broadcast/archive/other.flv?ticket={ticket}"),
    )
    .await;
    assert_eq!(other.status, 401);

    for body in [
        json!({}),
        json!({ "path": "" }),
        json!({ "path": "../movie.flv" }),
        json!({ "path": "a?b=c" }),
    ] {
        let response = harness.request("POST", mint_path, None, Some(&body)).await;
        assert_eq!(response.status, 400, "{body}");
    }
    let unknown = harness
        .request(
            "POST",
            "/api/v1/rtmp/vod/live/broadcast/missing/ticket",
            None,
            Some(&json!({ "path": "movie.flv" })),
        )
        .await;
    assert_eq!(unknown.status, 404);
}
