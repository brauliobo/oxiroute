use std::sync::Arc;

use tempfile::tempdir;

use super::*;
use crate::{DashSegmentNaming, LiveHub, LiveHubLimits, MediaEvent, MediaStoreLimits};

mod media_config_corpus {
    include!("../../tests/corpus/media_configs.rs");
}

fn test_config(store: Arc<MediaStore>) -> Arc<HlsOutputConfig> {
    Arc::new(HlsOutputConfig {
        store,
        segment_duration: Duration::from_secs(2),
        max_segment_duration: Duration::from_secs(10),
        playlist_length: Duration::from_secs(30),
        naming: HlsFragmentNaming::Sequential,
        nested: false,
        cleanup: true,
        variants: Vec::new(),
        keys: Some(HlsKeyConfig {
            rotation_segments: 1,
            url_prefix: "keys/".into(),
        }),
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 16,
    })
}

fn avc_sequence_header() -> Vec<u8> {
    vec![
        0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2, 0x68,
        0xce,
    ]
}

fn avc_frame(keyframe: bool) -> Vec<u8> {
    vec![
        if keyframe { 0x17 } else { 0x27 },
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        4,
        0x65,
        0x88,
        0x84,
        0x21,
    ]
}

fn segmenter(root: &Path) -> HlsSegmenter {
    let store = Arc::new(
        MediaStore::open(
            root.join("hls"),
            MediaStoreLimits {
                max_bytes: 4 * 1024 * 1024,
                max_files: 64,
                max_active_streams: 2,
                max_file_bytes: 1024 * 1024,
            },
        )
        .expect("HLS store"),
    );
    let hub = LiveHub::new(LiveHubLimits::default());
    let key = StreamKey::new("live", "broadcast", "camera");
    let lease = hub.attach_publisher(key.clone()).expect("publisher");
    store
        .attach(&key, lease.incarnation())
        .expect("HLS media incarnation");
    HlsSegmenter::new(
        key,
        lease.incarnation(),
        Arc::clone(&store),
        test_config(store),
    )
}

#[test]
fn characterizes_avc_configuration_acceptance() {
    for case in media_config_corpus::avc_cases() {
        assert_eq!(
            parse_avc_config(&case.payload).is_some(),
            case.accepted_by(media_config_corpus::Consumer::Hls),
            "{}",
            case.name,
        );
    }
}

#[test]
fn characterizes_aac_configuration_acceptance() {
    for case in media_config_corpus::aac_cases() {
        assert_eq!(
            parse_aac_config(&case.payload).is_some(),
            case.accepted_by(media_config_corpus::Consumer::Hls),
            "{}",
            case.name,
        );
    }
}

#[test]
fn recovers_from_invalid_headers_and_preserves_accepted_configs() {
    let root = tempdir().expect("HLS root");
    let mut segmenter = segmenter(root.path());
    let mut invalid_avc = avc_sequence_header();
    invalid_avc[5] = 2;
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(invalid_avc)).expect("invalid AVC event"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x2a, 0x10].as_slice()))
            .expect("invalid AAC event"),
        0,
    );
    assert!(segmenter.video_config.is_none());
    assert!(segmenter.audio_config.is_none());
    assert!(!segmenter.failed);

    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("valid AVC event"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x12, 0x10].as_slice()))
            .expect("valid AAC event"),
        0,
    );
    assert!(segmenter.video_config.is_some());
    assert!(segmenter.audio_config.is_some());

    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from([0x17, 0, 0, 0, 0, 2].as_slice()))
            .expect("invalid replacement AVC event"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x2a, 0x10].as_slice()))
            .expect("invalid replacement AAC event"),
        0,
    );
    assert!(segmenter.video_config.is_some());
    assert!(segmenter.audio_config.is_some());
    assert!(!segmenter.failed);
}

#[test]
fn avc_replacement_finishes_the_current_segment() {
    let root = tempdir().expect("HLS root");
    let mut segmenter = segmenter(root.path());
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x12, 0x10].as_slice()))
            .expect("AAC config"),
        0,
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("AVC config"),
        0,
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_frame(true))).expect("keyframe"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(1, Arc::<[u8]>::from([0xaf, 1, 1].as_slice())).expect("AAC frame"),
        1,
    );
    assert!(segmenter.current.is_some());

    let mut replacement = avc_sequence_header();
    replacement[16] = 1;
    segmenter.accept(
        &MediaEvent::video(2, Arc::<[u8]>::from(replacement)).expect("replacement AVC config"),
        2,
    );
    assert!(segmenter.current.is_none());
    assert_eq!(segmenter.published.len(), 1);
    assert_eq!(segmenter.next_sequence, 1);
}

#[test]
fn aac_replacement_updates_future_segments_without_a_boundary() {
    let root = tempdir().expect("HLS root");
    let mut segmenter = segmenter(root.path());
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x12, 0x10].as_slice()))
            .expect("AAC config"),
        0,
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("AVC config"),
        0,
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_frame(true))).expect("keyframe"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(1, Arc::<[u8]>::from([0xaf, 1, 1].as_slice())).expect("AAC frame"),
        1,
    );

    segmenter.accept(
        &MediaEvent::audio(2, Arc::<[u8]>::from([0xaf, 0, 0x11, 0x90].as_slice()))
            .expect("replacement AAC config"),
        2,
    );
    assert!(segmenter.current.is_some());
    assert!(segmenter.published.is_empty());
    assert_eq!(
        segmenter
            .audio_config
            .expect("replacement config")
            .sample_rate_index,
        3,
    );
}

#[test]
fn ignores_enhanced_avc_hevc_and_av1_headers() {
    let root = tempdir().expect("HLS root");
    let mut segmenter = segmenter(root.path());
    for four_cc in [*b"avc1", *b"hvc1", *b"av01"] {
        let mut payload = vec![0x90];
        payload.extend_from_slice(&four_cc);
        payload.push(1);
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(payload)).expect("enhanced header"),
            0,
        );
    }
    assert!(segmenter.video_config.is_none());
    assert!(!segmenter.failed);
}

#[test]
fn cuts_ts_segments_and_serves_encrypted_keys_from_the_playlist_path() {
    let root = tempdir().expect("temporary media root");
    let store = Arc::new(
        MediaStore::open(
            root.path().join("hls"),
            MediaStoreLimits {
                max_bytes: 1024 * 1024,
                max_files: 32,
                max_active_streams: 2,
                max_file_bytes: 1024 * 1024,
            },
        )
        .expect("media store"),
    );
    let hub = LiveHub::new(LiveHubLimits::default());
    let key = StreamKey::new("service", "application", "stream");
    let lease = hub.attach_publisher(key.clone()).expect("publisher");
    store
        .attach(&key, lease.incarnation())
        .expect("media incarnation");
    let mut segmenter = HlsSegmenter::new(
        key.clone(),
        lease.incarnation(),
        Arc::clone(&store),
        test_config(Arc::clone(&store)),
    );
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x12, 0x10].as_slice()))
            .expect("AAC sequence header"),
        0,
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header()))
            .expect("AVC sequence header"),
        0,
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_frame(true))).expect("keyframe"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 1, 1, 2, 3].as_slice())).expect("AAC frame"),
        0,
    );
    segmenter.accept(
        &MediaEvent::audio(1_000, Arc::<[u8]>::from([0xaf, 1, 4, 5, 6].as_slice()))
            .expect("AAC frame"),
        1_000,
    );
    segmenter.accept(
        &MediaEvent::video(1_000, Arc::<[u8]>::from(avc_frame(false))).expect("interframe"),
        1_000,
    );
    segmenter.accept(
        &MediaEvent::video(2_000, Arc::<[u8]>::from(avc_frame(true)))
            .expect("segment boundary keyframe"),
        2_000,
    );
    segmenter.finish(true);

    let prefix = store.current_prefix(&key).expect("current media prefix");
    let playlist = store
        .read_relative(&prefix.join("index.m3u8"), 1024)
        .expect("playlist");
    let playlist = String::from_utf8(playlist).expect("playlist UTF-8");
    assert!(playlist.contains("#EXTINF:"));
    assert!(playlist.contains("#EXT-X-ENDLIST"));
    assert!(playlist.contains("URI=\"keys/key-0.bin\""));
    let (key_path, _) =
        resolve_public_path(&test_config(Arc::clone(&store)), &prefix, "keys/key-0.bin")
            .expect("key path");
    assert_eq!(store.read_relative(&key_path, 16).expect("key").len(), 16);
    let segment_path = prefix.join("seg-0.ts");
    let segment = store
        .read_relative(&segment_path, 1024 * 1024)
        .expect("segment");
    assert!(!segment.is_empty() && segment.len().is_multiple_of(16));
}

#[test]
fn rejects_public_path_traversal() {
    let root = tempdir().expect("temporary media root");
    let store = Arc::new(
        MediaStore::open(
            root.path().join("hls"),
            MediaStoreLimits {
                max_bytes: 1024,
                max_files: 8,
                max_active_streams: 1,
                max_file_bytes: 512,
            },
        )
        .expect("media store"),
    );
    let config = test_config(store);
    let prefix = PathBuf::from("application/stream/i1");
    assert!(matches!(
        resolve_public_path(&config, &prefix, "../secret.ts"),
        Err(MediaStoreError::InvalidPath)
    ));
}

#[test]
fn releases_attached_stores_when_a_second_output_cannot_attach() {
    let root = tempdir().expect("temporary media root");
    let limits = MediaStoreLimits {
        max_bytes: 1024 * 1024,
        max_files: 16,
        max_active_streams: 1,
        max_file_bytes: 1024 * 1024,
    };
    let hls_store = Arc::new(MediaStore::open(root.path().join("hls"), limits).expect("HLS store"));
    let dash_store =
        Arc::new(MediaStore::open(root.path().join("dash"), limits).expect("DASH store"));
    let hub = LiveHub::new(LiveHubLimits::default());
    let occupied_key = StreamKey::new("service", "application", "occupied");
    let occupied = hub
        .attach_publisher(occupied_key.clone())
        .expect("occupied publisher");
    dash_store
        .attach(&occupied_key, occupied.incarnation())
        .expect("occupied DASH incarnation");

    let key = StreamKey::new("service", "application", "stream");
    let lease = hub.attach_publisher(key.clone()).expect("publisher");
    let dash = Arc::new(DashOutputConfig {
        store: Arc::clone(&dash_store),
        segment_duration: Duration::from_secs(1),
        max_segment_duration: Duration::from_secs(2),
        playlist_length: Duration::from_secs(4),
        naming: DashSegmentNaming::Sequential,
        nested: true,
        cleanup: true,
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 16,
    });
    let application =
        MediaApplication::new(Some(test_config(Arc::clone(&hls_store)))).with_dash(Some(dash));
    assert!(matches!(
        MediaPublisher::start(&key, lease.incarnation(), &application),
        Err(MediaOutputError::Storage(
            MediaStoreError::ActiveStreamLimit
        ))
    ));
    assert!(hls_store.current_prefix(&key).is_none());
    dash_store.close(&occupied_key, occupied.incarnation());
}
