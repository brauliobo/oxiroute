use std::{collections::BTreeMap, sync::Arc, thread, time::Duration};

use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

use super::*;
use crate::{
    LiveHub, LiveHubLimits, MediaEvent, RtmpCapabilities, RtmpRegistry, SessionId, StreamKey,
    VideoCodecIdentifier,
};
use rml_rtmp::sessions::StreamMetadata;

fn config(directory: &std::path::Path) -> RtmpAutoPushConfig {
    RtmpAutoPushConfig {
        enabled: true,
        socket_dir: directory.to_path_buf(),
        secret_file: None,
        reconnect_interval: Duration::from_millis(10),
        connect_timeout: Duration::from_millis(500),
        handshake_timeout: Duration::from_millis(500),
        max_peers: 4,
        max_queue_messages: 32,
        max_queue_bytes: 1024 * 1024,
        max_streams: 8,
    }
}

#[test]
fn authenticated_local_workers_copy_media_without_forwarding_side_effects() {
    let directory = tempdir().expect("auto-push socket directory");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("secure auto-push socket directory");
    let source_hub = LiveHub::new(LiveHubLimits::default());
    let target_hub = LiveHub::new(LiveHubLimits::default());
    let source_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let target_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }));
    let source = AutoPushCoordinator::new(
        config(directory.path()),
        "live",
        Arc::clone(&source_registry),
        source_hub.clone(),
        BTreeMap::new(),
    );
    let target = AutoPushCoordinator::new(
        config(directory.path()),
        "live",
        Arc::clone(&target_registry),
        target_hub.clone(),
        BTreeMap::new(),
    );
    target.ensure_started().expect("target worker socket");
    let key = StreamKey::new("live", "broadcast", "camera");
    let local_lease = source_hub
        .attach_publisher(key.clone())
        .expect("source publisher lease");
    let publisher = source
        .source(key.clone(), SessionId::new(), local_lease.incarnation())
        .expect("source admission")
        .expect("auto-push publisher");
    let subscription = target_hub
        .subscribe(key.clone())
        .expect("target subscriber");
    for _ in 0..100 {
        if source.status().peers > 0 && target.status().peers > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(source.status().peers, 1);
    assert_eq!(target.status().peers, 1);

    publish_snapshot_trace(&publisher);
    for _ in 0..100 {
        if target.status().remote_streams == 1
            && target_registry
                .snapshot()
                .streams
                .first()
                .is_some_and(|stream| {
                    stream.media.audio.payload_bytes_received == 3
                        && stream.media.video.payload_bytes_received == 6
                })
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        target.status().remote_streams,
        1,
        "source status: {:?}, target status: {:?}",
        source.status(),
        target.status()
    );
    assert!(subscription.try_next().is_some());
    assert_eq!(target.status().source_streams, 0);
    assert_remote_snapshot(&target_registry);

    assert_reverse_copy(&target, &source, &target_hub, &source_hub);
}

fn publish_snapshot_trace(publisher: &AutoPushPublisher) {
    let mut metadata = StreamMetadata::new();
    metadata.audio_codec_id = Some(10);
    metadata.video_codec_id = Some(7);
    publisher.publish(&MediaEvent::metadata(metadata).expect("metadata event"), 1);
    publisher.publish(
        &MediaEvent::audio(1, Arc::<[u8]>::from(&[0xaf, 1, 0x10][..])).expect("audio event"),
        2,
    );
    publisher.publish(
        &MediaEvent::video(
            2,
            Arc::<[u8]>::from(&[0x91, b'h', b'v', b'c', b'1', 0x20][..]),
        )
        .expect("video event"),
        3,
    );
}

fn assert_remote_snapshot(registry: &RtmpRegistry) {
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.streams.len(), 1);
    let media = snapshot.streams[0].media;
    assert_eq!(media.audio.flv_codec_id, Some(10));
    assert_eq!(media.audio.last_rtmp_timestamp_ms, Some(1));
    assert!(media.audio.last_observed_at_unix_ms.is_some());
    assert_eq!(
        media.video.video_codec,
        Some(VideoCodecIdentifier::FourCc(*b"hvc1"))
    );
    assert_eq!(media.video.last_rtmp_timestamp_ms, Some(2));
    assert!(media.video.last_observed_at_unix_ms.is_some());
    assert_eq!(media.fanout_payload_bytes_queued, 0);
}

fn assert_reverse_copy(
    target: &AutoPushCoordinator,
    source: &AutoPushCoordinator,
    target_hub: &LiveHub,
    source_hub: &LiveHub,
) {
    let key = StreamKey::new("live", "broadcast", "reverse");
    let lease = target_hub
        .attach_publisher(key.clone())
        .expect("reverse source publisher lease");
    let publisher = target
        .source(key.clone(), SessionId::new(), lease.incarnation())
        .expect("reverse source admission")
        .expect("reverse auto-push publisher");
    let subscription = source_hub
        .subscribe(key)
        .expect("reverse target subscription");
    publisher.publish(
        &MediaEvent::audio(2, Arc::<[u8]>::from(&[0xaf, 1, 0x20][..])).expect("audio event"),
        1,
    );
    for _ in 0..100 {
        if source.status().remote_streams == 1 && subscription.queued_messages() > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(source.status().remote_streams, 1);
    assert!(subscription.try_next().is_some());
}

#[test]
fn metadata_decode_is_best_effort_for_accepted_auto_push_frames() {
    let mut metadata = StreamMetadata::new();
    metadata.audio_codec_id = Some(10);
    metadata.video_codec_id = Some(7);
    let encoded = MediaEvent::metadata(metadata).expect("metadata event");
    let decoded = unix::decode_event(0, 0, encoded.payload().to_vec())
        .expect("structured metadata remains accepted");
    assert_eq!(
        decoded
            .stream_metadata()
            .and_then(|value| value.audio_codec_id),
        Some(10)
    );
    assert_eq!(
        decoded
            .stream_metadata()
            .and_then(|value| value.video_codec_id),
        Some(7)
    );

    let opaque = unix::decode_event(0, 0, vec![0xff]).expect("opaque metadata remains accepted");
    assert!(opaque.stream_metadata().is_none());
}
