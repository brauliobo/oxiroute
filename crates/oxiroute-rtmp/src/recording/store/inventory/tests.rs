use std::fs;

use tempfile::tempdir;

use super::*;

fn store(root: &Path) -> RecordingStore {
    RecordingStore::open(
        root,
        RecordingStoreLimits {
            max_bytes: None,
            max_files: None,
            max_active_recorders: 2,
            retention: RecordingRetention::default(),
        },
    )
    .expect("store")
}

fn complete_meta() -> RecordingMeta {
    RecordingMeta {
        complete: true,
        stream: Some("cam\nera\\1".into()),
        started_unix_ms: Some(1_000),
        ended_unix_ms: Some(3_000),
        duration_ms: Some(2_000),
        video_codec: Some("avc".into()),
        audio_codec: Some("aac".into()),
    }
}

#[test]
fn sidecar_text_round_trips_awkward_stream_names() {
    let meta = complete_meta();
    assert_eq!(RecordingMeta::decode(&meta.encode()), Some(meta));
    assert_eq!(RecordingMeta::decode("not a sidecar\n"), None);
}

#[test]
fn inventory_derives_status_from_sidecars_ownership_and_flv_tails() {
    let root = tempdir().expect("root");
    let store = store(root.path());
    let mut finished = store.create("done.flv").expect("create");
    finished.write_all(b"FLV").expect("write");
    finished.commit().expect("commit");
    store
        .write_sidecar("done.flv", &complete_meta())
        .expect("sidecar");
    fs::write(root.path().join("crashed.flv"), b"FLV").expect("crashed file");
    store
        .write_sidecar(
            "crashed.flv",
            &RecordingMeta {
                complete: false,
                ..complete_meta()
            },
        )
        .expect("crashed sidecar");
    fs::write(root.path().join("legacy.flv"), b"garbage").expect("legacy file");
    let active = store.create("live.flv").expect("active recording");

    let entries = store.inventory().expect("inventory");
    let status = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name == name)
            .expect("entry")
            .status
    };

    assert_eq!(status("done.flv"), RecordingStatus::Complete);
    assert_eq!(status("crashed.flv"), RecordingStatus::Partial);
    assert_eq!(status("legacy.flv"), RecordingStatus::Partial);
    assert_eq!(status("live.flv"), RecordingStatus::Recording);
    assert_eq!(entries.len(), 4);
    let done = entries
        .iter()
        .find(|entry| entry.name == "done.flv")
        .expect("done");
    assert_eq!(done.stream.as_deref(), Some("cam\nera\\1"));
    assert_eq!(done.duration_ms, Some(2_000));
    assert_eq!(done.audio_codec.as_deref(), Some("aac"));
    assert!(matches!(
        store.delete_recording("live.flv"),
        Err(RecordingStoreError::RecordingActive)
    ));
    drop(active);
}

#[test]
fn recording_ids_are_stable_per_root_and_unique_across_roots() {
    let first_root = tempdir().expect("first root");
    let second_root = tempdir().expect("second root");
    fs::write(first_root.path().join("a.flv"), b"x").expect("file");
    fs::write(second_root.path().join("a.flv"), b"x").expect("file");

    let first = store(first_root.path()).inventory().expect("first");
    let again = store(first_root.path()).inventory().expect("again");
    let second = store(second_root.path()).inventory().expect("second");

    assert_eq!(first[0].id, again[0].id);
    assert_ne!(first[0].id, second[0].id);
    assert_eq!(first[0].id.len(), 24);
}

#[test]
fn sidecars_do_not_count_as_recordings_or_quota_files() {
    let root = tempdir().expect("root");
    let store = store(root.path());
    let mut file = store.create("a.flv").expect("create");
    file.write_all(b"FLV").expect("write");
    file.commit().expect("commit");
    store
        .write_sidecar("a.flv", &complete_meta())
        .expect("sidecar");

    assert_eq!(store.inventory().expect("inventory").len(), 1);
    assert_eq!(store.delete_recording("a.flv").expect("delete"), 3);
    assert!(!root.path().join(sidecar_name("a.flv")).exists());
    assert!(store.create(&sidecar_name("a.flv")).is_err());
}
