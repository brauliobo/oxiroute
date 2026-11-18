use std::path::Path;
use std::time::{Duration, Instant};
use std::{fs, thread};

use oxiroute_rtmp::{
    MediaEvent, RecorderEnqueueResult, RecorderShutdown, RecorderWorker, RecorderWorkerConfig,
    RecordingDateTime, RecordingPathPolicy, RecordingStore, RecordingStoreLimits,
};

pub(super) fn store(root: &Path) -> RecordingStore {
    RecordingStore::open(
        root,
        RecordingStoreLimits {
            max_bytes: Some(1024 * 1024),
            max_files: Some(16),
            max_active_recorders: 2,
            retention: oxiroute_rtmp::RecordingRetention::default(),
        },
    )
    .expect("recording store")
}

pub(super) fn worker(
    store: &RecordingStore,
    rotation_interval: Option<Duration>,
    max_queue_bytes: usize,
) -> RecorderWorker {
    RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        RecorderWorkerConfig {
            max_queue_messages: 32,
            max_queue_bytes,
            rotation_interval,
            shutdown_timeout: Duration::from_secs(1),
            video_codec: None,
            ..RecorderWorkerConfig::default()
        },
    )
    .expect("recorder worker")
}

pub(super) fn worker_with_config(
    store: &RecordingStore,
    config: RecorderWorkerConfig,
) -> RecorderWorker {
    RecorderWorker::start(
        store.clone(),
        &RecordingPathPolicy::new(".flv", false).expect("path policy"),
        b"camera",
        1_721_657_969,
        RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("recording date-time"),
        config,
    )
    .expect("recorder worker")
}

pub(super) fn shutdown(worker: RecorderWorker) -> oxiroute_rtmp::RecorderWorkerStatus {
    match worker.shutdown() {
        RecorderShutdown::Joined(status) => status,
        RecorderShutdown::TimedOut(supervisor) => {
            panic!("recorder shutdown timed out: {:?}", supervisor.status())
        }
    }
}

pub(super) fn enqueue(worker: &RecorderWorker, event: MediaEvent) {
    assert_eq!(worker.try_enqueue(event), RecorderEnqueueResult::Queued);
}

pub(super) fn wait_for_processed(worker: &RecorderWorker, events: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while worker.status().events_processed < events {
        assert!(Instant::now() < deadline, "recorder processing timeout");
        thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn recording_files(root: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(root)
        .expect("recording entries")
        .map(|entry| entry.expect("recording entry"))
        .filter(|entry| entry.file_name() != ".oxiroute-recording.lock")
        .map(|entry| entry.path())
        .collect()
}

pub(super) fn aac_header(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, aac_payload(marker)).expect("AAC header")
}

pub(super) fn audio(timestamp_ms: u32, marker: u8) -> MediaEvent {
    MediaEvent::audio(timestamp_ms, audio_payload(marker)).expect("AAC audio")
}

pub(super) fn aac_payload(marker: u8) -> Vec<u8> {
    vec![0xaf, 0x00, marker, 0x10]
}

pub(super) fn audio_payload(marker: u8) -> Vec<u8> {
    vec![0xaf, 0x01, marker]
}

pub(super) fn parse_tags(bytes: &[u8]) -> Vec<(u8, u32, Vec<u8>)> {
    assert_eq!(&bytes[..3], b"FLV");
    let mut tags = Vec::new();
    let mut offset = 13;
    while offset < bytes.len() {
        let size = usize::from_be_bytes([
            0,
            0,
            0,
            0,
            0,
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        let timestamp = u32::from_be_bytes([
            bytes[offset + 7],
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
        ]);
        let payload_start = offset + 11;
        let payload_end = payload_start + size;
        tags.push((
            bytes[offset],
            timestamp,
            bytes[payload_start..payload_end].to_vec(),
        ));
        offset = payload_end + 4;
    }
    tags
}

pub(super) fn payloads(tags: &[(u8, u32, Vec<u8>)]) -> Vec<Vec<u8>> {
    tags.iter().map(|tag| tag.2.clone()).collect()
}
