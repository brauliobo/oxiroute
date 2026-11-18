use std::{
    fs::{self, File},
    time::{Duration, SystemTime},
};

use tempfile::tempdir;

use super::*;

fn limits(files: usize, bytes: u64, retention: RecordingRetention) -> RecordingStoreLimits {
    RecordingStoreLimits {
        max_bytes: Some(bytes),
        max_files: Some(files),
        max_active_recorders: 2,
        retention,
    }
}

fn delete_oldest() -> RecordingRetention {
    RecordingRetention {
        delete_oldest: true,
        max_age: None,
    }
}

fn record(store: &RecordingStore, name: &str, bytes: usize) -> Result<(), RecordingStoreError> {
    let mut file = store.create(name)?;
    file.write_all(&vec![7; bytes])
        .map_err(RecordingStoreError::PartialCreate)?;
    file.commit().map(|_| ())
}

fn age(root: &Path, name: &str, seconds_ago: u64) {
    File::options()
        .write(true)
        .open(root.join(name))
        .expect("recording")
        .set_modified(SystemTime::now() - Duration::from_secs(seconds_ago))
        .expect("modification time");
}

#[test]
fn file_quota_without_retention_names_the_limit_and_recovers_when_space_is_freed() {
    let root = tempdir().expect("root");
    let store = RecordingStore::open(root.path(), limits(2, 4096, RecordingRetention::default()))
        .expect("store");
    record(&store, "a.flv", 8).expect("first");
    record(&store, "b.flv", 8).expect("second");

    assert!(matches!(
        store.create("c.flv").err(),
        Some(RecordingStoreError::FileLimit { maximum: 2 })
    ));
    fs::remove_file(root.path().join("a.flv")).expect("free one file");

    record(&store, "c.flv", 8).expect("created after space was freed");
    assert_eq!(store.evictions(), RecordingEvictions::default());
}

#[test]
fn delete_oldest_keeps_the_newest_files_within_the_file_quota() {
    let root = tempdir().expect("root");
    let store = RecordingStore::open(root.path(), limits(2, 4096, delete_oldest())).expect("store");
    record(&store, "a.flv", 8).expect("a");
    record(&store, "b.flv", 8).expect("b");
    age(root.path(), "a.flv", 300);
    age(root.path(), "b.flv", 200);
    fs::write(root.path().join(sidecar_name("a.flv")), b"{}").expect("sidecar");

    record(&store, "c.flv", 8).expect("c evicts the oldest");

    assert!(!root.path().join("a.flv").exists());
    assert!(!root.path().join(sidecar_name("a.flv")).exists());
    assert!(root.path().join("b.flv").exists());
    assert!(root.path().join("c.flv").exists());
    assert_eq!(store.evictions(), RecordingEvictions { files: 1, bytes: 8 });
    assert_eq!(store.stats().files, 2);
}

#[test]
fn delete_oldest_never_deletes_an_active_recording() {
    let root = tempdir().expect("root");
    let store = RecordingStore::open(root.path(), limits(1, 4096, delete_oldest())).expect("store");
    let active = store.create("a.flv").expect("active recording");

    assert!(matches!(
        store.create("b.flv").err(),
        Some(RecordingStoreError::FileLimit { maximum: 1 })
    ));
    assert!(root.path().join("a.flv").exists());
    drop(active);
}

#[test]
fn byte_quota_without_retention_reports_the_root_quota() {
    let root = tempdir().expect("root");
    let store = RecordingStore::open(root.path(), limits(8, 100, RecordingRetention::default()))
        .expect("store");
    record(&store, "a.flv", 60).expect("a");

    let mut file = store.create("b.flv").expect("b");
    let error = file.write_all(&[1; 60]).expect_err("quota");

    assert!(is_root_byte_quota_error(&error));
}

#[test]
fn delete_oldest_frees_bytes_for_a_growing_recording() {
    let root = tempdir().expect("root");
    let store = RecordingStore::open(root.path(), limits(8, 100, delete_oldest())).expect("store");
    record(&store, "a.flv", 60).expect("a");
    age(root.path(), "a.flv", 100);

    record(&store, "b.flv", 60).expect("b evicts a");

    assert!(!root.path().join("a.flv").exists());
    assert_eq!(
        store.evictions(),
        RecordingEvictions {
            files: 1,
            bytes: 60
        }
    );
}

#[test]
fn recordings_older_than_the_retention_age_are_deleted_when_a_recording_starts() {
    let root = tempdir().expect("root");
    let retention = RecordingRetention {
        delete_oldest: false,
        max_age: Some(Duration::from_hours(1)),
    };
    let store = RecordingStore::open(root.path(), limits(8, 4096, retention)).expect("store");
    record(&store, "old.flv", 8).expect("old");
    record(&store, "fresh.flv", 8).expect("fresh");
    age(root.path(), "old.flv", 7200);

    record(&store, "next.flv", 8).expect("next");

    assert!(!root.path().join("old.flv").exists());
    assert!(root.path().join("fresh.flv").exists());
    assert_eq!(store.evictions().files, 1);
}
