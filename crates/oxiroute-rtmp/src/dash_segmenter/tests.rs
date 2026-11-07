use std::sync::Arc;

use tempfile::tempdir;

use super::*;
use crate::{LiveHub, LiveHubLimits, MediaStoreLimits};

mod media_config_corpus {
    include!("../../tests/corpus/media_configs.rs");
}

fn avc_sequence_header() -> Vec<u8> {
    vec![
        0x17, 0, 0, 0, 0, 1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0, 0x1e, 1, 0, 2, 0x68,
        0xce,
    ]
}

fn avc_frame(keyframe: bool, marker: u8) -> Vec<u8> {
    vec![
        if keyframe { 0x17 } else { 0x27 },
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        2,
        if keyframe { 0x65 } else { 0x41 },
        marker,
    ]
}

fn aac_sequence_header() -> Vec<u8> {
    vec![0xaf, 0, 0x12, 0x10]
}

fn aac_frame(marker: u8) -> Vec<u8> {
    vec![0xaf, 1, marker, 0x21, 0x10]
}

fn config(store: Arc<MediaStore>) -> Arc<DashOutputConfig> {
    Arc::new(DashOutputConfig {
        store,
        segment_duration: Duration::from_secs(1),
        max_segment_duration: Duration::from_secs(2),
        playlist_length: Duration::from_secs(4),
        naming: DashSegmentNaming::Sequential,
        nested: true,
        cleanup: true,
        max_segment_bytes: 1024 * 1024,
        max_queue_messages: 16,
    })
}

fn segmenter(root: &std::path::Path) -> (DashSegmenter, Arc<MediaStore>, StreamKey) {
    let store = Arc::new(
        MediaStore::open(
            root.join("dash"),
            MediaStoreLimits {
                max_bytes: 4 * 1024 * 1024,
                max_files: 64,
                max_active_streams: 2,
                max_file_bytes: 1024 * 1024,
            },
        )
        .expect("DASH store"),
    );
    let hub = LiveHub::new(LiveHubLimits::default());
    let key = StreamKey::new("live", "broadcast", "camera");
    let lease = hub.attach_publisher(key.clone()).expect("publisher");
    store
        .attach_continuing(&key, lease.incarnation())
        .expect("DASH media incarnation");
    let segmenter = DashSegmenter::new(
        key.clone(),
        lease.incarnation(),
        Arc::clone(&store),
        config(Arc::clone(&store)),
    )
    .expect("DASH segmenter");
    (segmenter, store, key)
}

#[test]
fn characterizes_avc_configuration_acceptance() {
    for case in media_config_corpus::avc_cases() {
        assert_eq!(
            parse_avc_config(&case.payload).is_some(),
            case.accepted_by(media_config_corpus::Consumer::Dash),
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
            case.accepted_by(media_config_corpus::Consumer::Dash),
            "{}",
            case.name,
        );
    }
}

#[test]
fn invalid_configuration_permanently_fails_the_segmenter() {
    let root = tempdir().expect("DASH root");
    let (mut segmenter, store, key) = segmenter(root.path());
    let mut invalid = avc_sequence_header();
    invalid[5] = 2;
    segmenter.accept(&MediaEvent::video(0, Arc::<[u8]>::from(invalid)).expect("invalid event"));
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("valid AVC config"),
    );
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header())).expect("valid AAC config"),
    );
    segmenter.finish(true);

    assert!(segmenter.failed);
    assert!(segmenter.video_config.is_none());
    assert!(segmenter.audio_config.is_none());
    let prefix = store.current_prefix(&key).expect("current prefix");
    assert!(
        store
            .read_relative(&prefix.join("dash/init.mp4"), 1024)
            .is_err()
    );
}

#[test]
fn changed_avc_or_aac_configuration_permanently_fails_the_segmenter() {
    {
        let root = tempdir().expect("DASH AVC root");
        let (mut segmenter, _, _) = segmenter(root.path());
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("AVC config"),
        );
        let mut changed_avc = avc_sequence_header();
        changed_avc[6] = 0x4d;
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(changed_avc)).expect("changed AVC config"),
        );
        assert!(segmenter.failed);
    }

    {
        let root = tempdir().expect("DASH AAC root");
        let (mut segmenter, _, _) = segmenter(root.path());
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header())).expect("AAC config"),
        );
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from([0xaf, 0, 0x11, 0x90].as_slice()))
                .expect("changed AAC config"),
        );
        assert!(segmenter.failed);
    }
}

#[test]
fn repeatedly_accepts_identical_configurations_but_retains_aac_trailing_bytes() {
    let root = tempdir().expect("DASH root");
    let (mut segmenter, _, _) = segmenter(root.path());
    for _ in 0..2 {
        segmenter.accept(
            &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header()))
                .expect("identical AVC config"),
        );
        segmenter.accept(
            &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header()))
                .expect("identical AAC config"),
        );
    }
    assert!(!segmenter.failed);
    assert!(segmenter.initialization_published);

    let mut trailing = aac_sequence_header();
    trailing.push(0xaa);
    segmenter
        .accept(&MediaEvent::audio(0, Arc::<[u8]>::from(trailing)).expect("AAC trailing bytes"));
    assert!(segmenter.failed);
}

#[test]
fn enhanced_avc_hevc_and_av1_headers_permanently_fail_the_segmenter() {
    for four_cc in [*b"avc1", *b"hvc1", *b"av01"] {
        let root = tempdir().expect("DASH root");
        let (mut segmenter, _, _) = segmenter(root.path());
        let mut payload = vec![0x90];
        payload.extend_from_slice(&four_cc);
        payload.push(1);
        segmenter
            .accept(&MediaEvent::video(0, Arc::<[u8]>::from(payload)).expect("enhanced header"));
        assert!(segmenter.failed, "{}", four_cc.escape_ascii());
    }
}

#[test]
fn writes_player_parseable_fragmented_mp4_and_bounded_mpd() {
    let root = tempdir().expect("DASH root");
    let (mut segmenter, store, key) = segmenter(root.path());
    segmenter.accept(
        &MediaEvent::audio(0, Arc::<[u8]>::from(aac_sequence_header())).expect("AAC config"),
    );
    segmenter.accept(
        &MediaEvent::video(0, Arc::<[u8]>::from(avc_sequence_header())).expect("AVC config"),
    );
    segmenter
        .accept(&MediaEvent::video(0, Arc::<[u8]>::from(avc_frame(true, 1))).expect("keyframe"));
    segmenter.accept(&MediaEvent::audio(0, Arc::<[u8]>::from(aac_frame(2))).expect("AAC frame"));
    segmenter.accept(
        &MediaEvent::video(1_000, Arc::<[u8]>::from(avc_frame(false, 3))).expect("interframe"),
    );
    segmenter.accept(
        &MediaEvent::video(2_000, Arc::<[u8]>::from(avc_frame(true, 4)))
            .expect("boundary keyframe"),
    );
    segmenter.finish(true);

    let prefix = store.current_prefix(&key).expect("current prefix");
    let directory = prefix.join("dash");
    let init = store
        .read_relative(&directory.join("init.mp4"), 1024 * 1024)
        .expect("initialization");
    let fragment = store
        .read_relative(&directory.join("seg-0.m4s"), 1024 * 1024)
        .expect("fragment");
    let manifest = store
        .read_relative(&directory.join("manifest.mpd"), 1024 * 1024)
        .expect("manifest");
    assert_eq!(&init[4..8], b"ftyp");
    assert!(contains_box(&init, *b"moov"));
    assert!(contains_box(&fragment, *b"moof"));
    assert!(contains_box(&fragment, *b"mdat"));
    let manifest = String::from_utf8(manifest).expect("MPD UTF-8");
    assert!(manifest.contains("type=\"static\""));
    assert!(manifest.contains("mimeType=\"video/mp4\""));
    assert!(manifest.contains("media=\"seg-0.m4s\""));
    assert!(!manifest.contains(".ts"));
}

#[test]
fn rejects_unsupported_codec_forms_before_publishing_dash_files() {
    let root = tempdir().expect("DASH root");
    let (mut segmenter, store, key) = segmenter(root.path());
    let mut invalid = avc_sequence_header();
    invalid[9] = 0xfe;
    segmenter.accept(&MediaEvent::video(0, Arc::<[u8]>::from(invalid)).expect("event"));
    segmenter.finish(true);
    let prefix = store.current_prefix(&key).expect("current prefix");
    assert!(
        store
            .read_relative(&prefix.join("dash/manifest.mpd"), 1024)
            .is_err()
    );
}

#[test]
fn parses_only_safe_segment_names() {
    assert!(valid_segment_filename("seg-42.m4s"));
    assert!(valid_segment_filename("seg-100-42.m4s"));
    assert!(!valid_segment_filename("../seg-42.m4s"));
    assert!(!valid_segment_filename("seg-42.ts"));
    assert!(!valid_segment_filename("seg-x.m4s"));
}

#[test]
fn rejects_malformed_persisted_manifest() {
    let root = tempdir().expect("DASH root");
    let (segmenter, store, key) = segmenter(root.path());
    let prefix = store.current_prefix(&key).expect("current prefix");
    store
        .publish(
            &key,
            segmenter.incarnation,
            &prefix.join("dash/manifest.mpd"),
            b"not an MPD",
        )
        .expect("malformed manifest");
    let result = DashSegmenter::new(
        key,
        segmenter.incarnation,
        Arc::clone(&store),
        config(Arc::clone(&store)),
    );
    drop(segmenter);
    assert!(matches!(result, Err(MediaStoreError::ManifestMalformed)));
}

fn contains_box(bytes: &[u8], kind: [u8; 4]) -> bool {
    bytes.windows(4).any(|window| window == kind)
}
