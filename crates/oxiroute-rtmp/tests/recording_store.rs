use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{PermissionsExt, symlink},
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use oxiroute_rtmp::{
    MAX_RECORDING_FILENAME_BYTES, RecordingQuotaScope, RecordingStore, RecordingStoreError,
    RecordingStoreLimits, RecordingStoreStats,
};
use rustix::fs::{FlockOperation, flock};
use tempfile::tempdir;

#[test]
fn rejects_a_symlink_root_and_pins_a_replaced_root_by_descriptor() {
    let temporary = tempdir().expect("temporary directory");
    let real_root = temporary.path().join("real");
    let root_link = temporary.path().join("link");
    fs::create_dir(&real_root).expect("real root");
    symlink(&real_root, &root_link).expect("root symlink");
    assert!(matches!(
        RecordingStore::open(&root_link, limits()),
        Err(RecordingStoreError::RootOpen(_))
    ));

    let configured_root = temporary.path().join("configured");
    let pinned_root = temporary.path().join("pinned");
    fs::create_dir(&configured_root).expect("configured root");
    let store = RecordingStore::open(&configured_root, limits()).expect("pinned store");
    fs::rename(&configured_root, &pinned_root).expect("move pinned root");
    fs::create_dir(&configured_root).expect("replacement root");

    let mut recording = store.create("camera.flv").expect("recording partial");
    recording.write_all(b"pinned").expect("recording data");
    let committed = recording.commit().expect("publish recording");

    assert_eq!(committed.relative_name, "camera.flv");
    assert_eq!(
        fs::read(pinned_root.join("camera.flv")).expect("pinned recording"),
        b"pinned"
    );
    assert!(
        fs::read_dir(&configured_root)
            .expect("replacement root entries")
            .next()
            .is_none()
    );
}

#[test]
fn rejects_a_symlink_in_an_intermediate_root_component() {
    let temporary = tempdir().expect("temporary directory");
    let real = temporary.path().join("real");
    let nested = real.join("nested");
    let link = temporary.path().join("link");
    fs::create_dir_all(&nested).expect("nested real root");
    symlink(&real, &link).expect("intermediate symlink");

    assert!(matches!(
        RecordingStore::open(link.join("nested"), limits()),
        Err(RecordingStoreError::RootOpen(_))
    ));
}

#[test]
fn does_not_follow_or_replace_a_colliding_final_symlink() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path().join("recordings");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).expect("recording root");
    fs::write(&outside, b"untouched").expect("outside file");
    symlink(&outside, root.join("camera.flv")).expect("colliding symlink");
    let store = RecordingStore::open(&root, limits()).expect("recording store");

    let mut recording = store.create("camera.flv").expect("recording partial");
    recording.write_all(b"recording").expect("recording data");
    let committed = recording.commit().expect("collision-safe publish");

    assert_eq!(committed.relative_name, "camera-1.flv");
    assert_eq!(fs::read(&outside).expect("outside content"), b"untouched");
    assert!(
        fs::symlink_metadata(root.join("camera.flv"))
            .expect("original symlink")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(root.join(committed.relative_name)).expect("published collision file"),
        b"recording"
    );
}

#[test]
fn rejects_a_root_writable_by_group_or_other_users() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path().join("recordings");
    fs::create_dir(&root).expect("recording root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o770)).expect("insecure root mode");

    assert!(matches!(
        RecordingStore::open(&root, limits()),
        Err(RecordingStoreError::RootNotExclusive)
    ));
}

#[test]
fn preflight_validates_usage_without_mutating_the_recording_root() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path();
    let stale_partial = ".oxiroute-recording-0123456789abcdef0123456789abcdef.partial";
    fs::write(root.join("existing.flv"), b"1234").expect("existing recording");
    fs::write(root.join(stale_partial), b"partial").expect("stale partial");
    let before = recording_entry_names(root);

    let stats = RecordingStore::preflight(root, limits()).expect("read-only preflight");

    assert_eq!(stats.bytes_used, 11);
    assert_eq!(stats.files, 2);
    assert_eq!(stats.active_recorders, 0);
    assert_eq!(recording_entry_names(root), before);
    assert!(!root.join(".oxiroute-recording.lock").exists());
}

#[test]
fn preflight_rejects_existing_quota_without_cleanup_or_lock_creation() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path();
    let partial = ".oxiroute-recording-0123456789abcdef0123456789abcdef.partial";
    fs::write(root.join(partial), b"12345").expect("partial");
    let before = recording_entry_names(root);

    assert!(matches!(
        RecordingStore::preflight(
            root,
            RecordingStoreLimits {
                max_bytes: Some(4),
                ..limits()
            }
        ),
        Err(RecordingStoreError::ExistingUsageExceedsLimits {
            bytes_used: 5,
            files: 1
        })
    ));
    assert_eq!(recording_entry_names(root), before);
    assert!(!root.join(".oxiroute-recording.lock").exists());
}

#[test]
fn omitted_storage_quotas_accept_existing_usage_and_new_growth() {
    let temporary = tempdir().expect("temporary directory");
    let existing = fs::File::create(temporary.path().join("existing.flv")).expect("existing file");
    existing
        .set_len(11 * 1024 * 1024 * 1024)
        .expect("sparse existing recording");
    let limits = RecordingStoreLimits {
        max_bytes: None,
        max_files: None,
        max_active_recorders: 8,
    };

    let stats = RecordingStore::preflight(temporary.path(), limits).expect("unbounded preflight");
    assert_eq!(stats.bytes_used, 11 * 1024 * 1024 * 1024);
    assert_eq!(stats.files, 1);

    let store = RecordingStore::open(temporary.path(), limits).expect("unbounded store");
    let mut recording = store.create("next.flv").expect("new recording");
    recording.write_all(b"growth").expect("unbounded growth");
    recording.commit().expect("committed growth");
    assert_eq!(store.stats().files, 2);
}

#[test]
fn legacy_lock_entry_replacement_does_not_interrupt_the_pinned_store() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(temporary.path(), limits()).expect("recording store");
    let lock = temporary.path().join(".oxiroute-recording.lock");
    let alias = temporary.path().join("lock-alias");
    fs::write(&lock, b"legacy").expect("legacy lock entry");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).expect("legacy lock mode");
    fs::hard_link(&lock, &alias).expect("hard-linked legacy lock");

    let first = store.create("camera.flv").expect("first recording");
    drop(first);
    fs::remove_file(&alias).expect("remove lock alias");
    fs::remove_file(&lock).expect("remove legacy lock");
    fs::write(&lock, b"replacement").expect("replacement regular lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).expect("replacement lock mode");
    let second = store.create("camera.flv").expect("second recording");
    drop(second);
    fs::remove_file(&lock).expect("remove replacement lock");
    symlink("lock-alias", &lock).expect("replacement lock symlink");
    let third = store.create("camera.flv").expect("third recording");
    drop(third);
    assert_eq!(store.stats(), RecordingStoreStats::default());
}

#[test]
fn refuses_to_publish_when_the_owned_recording_name_is_replaced_by_a_symlink() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path().join("recordings");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).expect("recording root");
    fs::write(&outside, b"untouched").expect("outside file");
    let store = RecordingStore::open(&root, limits()).expect("recording store");
    let mut recording = store.create("camera.flv").expect("recording partial");
    recording.write_all(b"recording").expect("recording data");
    let recording_path = root.join("camera.flv");
    fs::remove_file(&recording_path).expect("remove owned recording name");
    symlink(&outside, &recording_path).expect("replacement symlink");

    assert!(matches!(
        recording.commit(),
        Err(RecordingStoreError::PartialOwnershipLost { .. })
    ));
    assert_eq!(fs::read(&outside).expect("outside content"), b"untouched");
    assert!(!root.join("camera-1.flv").exists());
    assert!(
        fs::symlink_metadata(recording_path)
            .expect("replacement remains")
            .file_type()
            .is_symlink()
    );
    assert_eq!(store.stats(), RecordingStoreStats::default());
}

#[test]
fn cleans_only_exact_regular_owned_partial_names() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path();
    let owned = ".oxiroute-recording-0123456789abcdef0123456789abcdef.partial";
    let uppercase = ".oxiroute-recording-0123456789ABCDEF0123456789ABCDEF.partial";
    let short = ".oxiroute-recording-0123456789abcdef.partial";
    let foreign = ".camera.partial";
    let symlink_partial = ".oxiroute-recording-fedcba9876543210fedcba9876543210.partial";
    fs::write(root.join(owned), b"stale").expect("owned partial");
    fs::write(root.join(uppercase), b"foreign").expect("uppercase lookalike");
    fs::write(root.join(short), b"foreign").expect("short lookalike");
    fs::write(root.join(foreign), b"foreign").expect("foreign partial");
    symlink("missing", root.join(symlink_partial)).expect("partial-shaped symlink");

    let store = RecordingStore::open(root, limits()).expect("recording store");

    assert!(!root.join(owned).exists());
    assert!(root.join(uppercase).exists());
    assert!(root.join(short).exists());
    assert!(root.join(foreign).exists());
    assert!(
        fs::symlink_metadata(root.join(symlink_partial))
            .expect("partial-shaped symlink retained")
            .file_type()
            .is_symlink()
    );
    assert_eq!(store.stats().files, 3);
}

#[test]
fn startup_scan_waits_for_exclusive_cross_process_cleanup() {
    let temporary = tempdir().expect("temporary directory");
    let partial = ".oxiroute-recording-0123456789abcdef0123456789abcdef.partial";
    fs::write(temporary.path().join(partial), b"stale").expect("stale partial");
    let ownership = fs::File::open(temporary.path()).expect("recording root ownership");
    flock(&ownership, FlockOperation::LockExclusive).expect("exclusive cleanup ownership");
    let root = temporary.path().to_owned();
    let (opened_tx, opened_rx) = mpsc::channel();
    let opener = thread::spawn(move || {
        let store = RecordingStore::open(root, limits()).expect("recording store");
        opened_tx.send(store.stats()).expect("report opened store");
    });

    assert!(matches!(
        opened_rx.recv_timeout(Duration::from_millis(40)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    fs::remove_file(temporary.path().join(partial)).expect("exclusive stale cleanup");
    flock(&ownership, FlockOperation::Unlock).expect("release cleanup ownership");

    assert_eq!(
        opened_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("store opens after cleanup"),
        RecordingStoreStats::default()
    );
    opener.join().expect("store opener");
}

#[test]
fn opening_a_second_store_never_cleans_an_active_writers_recording() {
    let temporary = tempdir().expect("temporary directory");
    let first_store = RecordingStore::open(temporary.path(), limits()).expect("first store");
    let mut first = first_store.create("camera.flv").expect("active recording");
    first.write_all(b"first").expect("first data");
    let active_recording = temporary.path().join("camera.flv");

    let second_store = RecordingStore::open(temporary.path(), limits()).expect("second store");
    assert!(active_recording.exists(), "live writer lost its recording");
    let mut second = second_store
        .create("camera.flv")
        .expect("concurrent recording partial");
    second.write_all(b"second").expect("second data");

    let barrier = Arc::new(Barrier::new(3));
    let first_thread = commit_after_barrier(first, Arc::clone(&barrier));
    let second_thread = commit_after_barrier(second, Arc::clone(&barrier));
    barrier.wait();
    let first = first_thread.join().expect("first writer thread");
    let second = second_thread.join().expect("second writer thread");
    let mut names = [first.relative_name, second.relative_name];
    names.sort();
    assert_eq!(names, ["camera-1.flv", "camera.flv"]);
    let mut lengths = [
        fs::read(temporary.path().join("camera.flv")).unwrap().len(),
        fs::read(temporary.path().join("camera-1.flv"))
            .unwrap()
            .len(),
    ];
    lengths.sort_unstable();
    assert_eq!(lengths, [5, 6]);
}

#[test]
fn independently_opened_stores_share_process_wide_quota_state() {
    let temporary = tempdir().expect("temporary directory");
    let limits = RecordingStoreLimits {
        max_bytes: Some(5),
        max_files: Some(1),
        max_active_recorders: 1,
    };
    let first_store = RecordingStore::open(temporary.path(), limits).expect("first store");
    let second_store = RecordingStore::open(temporary.path(), limits).expect("second store");
    let lease = first_store
        .acquire_recorder()
        .expect("first recorder lease");
    let mut recording = first_store.create("camera.flv").expect("first recording");

    assert_eq!(second_store.stats().active_recorders, 1);
    assert!(matches!(
        second_store.acquire_recorder(),
        Err(RecordingStoreError::ActiveRecorderLimit { maximum: 1 })
    ));
    recording.write_all(b"12345").expect("quota-sized data");
    assert_eq!(second_store.stats().bytes_used, 5);
    recording.commit().expect("published recording");
    assert_eq!(second_store.stats().files, 1);
    assert!(matches!(
        second_store.create("full.flv"),
        Err(RecordingStoreError::FileLimit { maximum: 1 })
    ));
    drop(lease);
    assert_eq!(second_store.stats().active_recorders, 0);
}

#[test]
fn reopening_one_root_with_different_limits_fails_closed() {
    let temporary = tempdir().expect("temporary directory");
    let _store = RecordingStore::open(temporary.path(), limits()).expect("first store");
    let different = RecordingStoreLimits {
        max_bytes: Some(1),
        ..limits()
    };

    assert!(matches!(
        RecordingStore::open(temporary.path(), different),
        Err(RecordingStoreError::LimitsMismatch)
    ));
}

#[test]
fn documents_that_quota_accounting_is_process_scoped() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(temporary.path(), limits()).expect("recording store");

    assert_eq!(store.quota_scope(), RecordingQuotaScope::Process);
}

#[test]
fn enforces_recorder_file_and_growth_quotas_and_releases_aborted_partials() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(5),
            max_files: Some(1),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    let lease = store.acquire_recorder().expect("first recorder lease");
    let mut recording = store.create("camera.flv").expect("first partial");
    assert!(matches!(
        store.acquire_recorder(),
        Err(RecordingStoreError::ActiveRecorderLimit { maximum: 1 })
    ));

    recording.write_all(b"12345").expect("quota-sized write");
    let error = recording
        .write_all(b"6")
        .expect_err("growth over quota must fail before writing");
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    recording.seek(SeekFrom::Start(0)).expect("rewind partial");
    recording
        .write_all(b"x")
        .expect("an in-place rewrite does not consume quota");
    assert_eq!(
        store.stats(),
        RecordingStoreStats {
            bytes_used: 5,
            files: 1,
            active_recorders: 1,
        }
    );

    drop(recording);
    assert_eq!(store.stats().active_recorders, 1);
    drop(lease);
    assert_eq!(store.stats(), RecordingStoreStats::default());
    assert!(recording_entries(temporary.path()).is_empty());
}

#[test]
fn counts_existing_regular_files_and_reserves_committed_file_slots() {
    let temporary = tempdir().expect("temporary directory");
    fs::write(temporary.path().join("existing.flv"), b"123").expect("existing recording");
    symlink("missing", temporary.path().join("ignored-link")).expect("unfollowed symlink");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(8),
            max_files: Some(2),
            max_active_recorders: 1,
        },
    )
    .expect("recording store");
    assert_eq!(store.stats().bytes_used, 3);
    assert_eq!(store.stats().files, 1);

    let mut recording = store.create("new.flv").expect("remaining file slot");
    recording.write_all(b"12345").expect("remaining byte quota");
    recording.commit().expect("publish recording");
    assert_eq!(store.stats().bytes_used, 8);
    assert_eq!(store.stats().files, 2);
    assert!(matches!(
        store.create("full.flv"),
        Err(RecordingStoreError::FileLimit { maximum: 2 })
    ));
}

#[test]
fn publishes_same_requested_name_without_overwrite_and_bounds_collision_names() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(
        temporary.path(),
        RecordingStoreLimits {
            max_bytes: Some(64),
            max_files: Some(3),
            max_active_recorders: 2,
        },
    )
    .expect("recording store");
    let requested = "x".repeat(MAX_RECORDING_FILENAME_BYTES);

    let mut first = store.create(&requested).expect("first partial");
    let mut second = store.create(&requested).expect("second partial");
    assert!(temporary.path().join(&requested).is_file());
    assert!(
        temporary
            .path()
            .join(format!(
                "{}-1",
                "x".repeat(MAX_RECORDING_FILENAME_BYTES - 2)
            ))
            .is_file()
    );
    first.write_all(b"first").expect("first data");
    second.write_all(b"second").expect("second data");
    let first = first.commit().expect("first publish");
    let second = second.commit().expect("second publish");

    assert_eq!(first.relative_name, requested);
    assert_eq!(
        second.relative_name,
        format!("{}-1", "x".repeat(MAX_RECORDING_FILENAME_BYTES - 2))
    );
    assert!(second.relative_name.len() <= MAX_RECORDING_FILENAME_BYTES);
    assert_eq!(
        fs::read(temporary.path().join(first.relative_name)).expect("first recording"),
        b"first"
    );
    assert_eq!(
        fs::read(temporary.path().join(second.relative_name)).expect("second recording"),
        b"second"
    );
}

#[test]
fn durable_commit_is_visible_to_a_fresh_descriptor_pinned_store() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(temporary.path(), limits()).expect("recording store");
    let mut recording = store.create("camera.flv").expect("recording partial");
    recording.write_all(b"durable").expect("recording data");
    assert_eq!(
        recording.commit().expect("durable publish").relative_name,
        "camera.flv"
    );
    drop(store);

    let reopened = RecordingStore::open(temporary.path(), limits()).expect("reopened store");
    assert_eq!(reopened.stats().bytes_used, 7);
    assert_eq!(reopened.stats().files, 1);
    assert_eq!(
        fs::read(temporary.path().join("camera.flv")).expect("published bytes"),
        b"durable"
    );
    assert!(
        recording_entries(temporary.path())
            .iter()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".partial"))
    );
}

#[test]
fn rejects_non_component_final_names() {
    let temporary = tempdir().expect("temporary directory");
    let store = RecordingStore::open(temporary.path(), limits()).expect("recording store");

    for invalid in ["", ".", "..", "nested/camera.flv", "camera\0.flv"] {
        assert!(matches!(
            store.create(invalid),
            Err(RecordingStoreError::InvalidRelativeName)
        ));
    }
}

fn limits() -> RecordingStoreLimits {
    RecordingStoreLimits {
        max_bytes: Some(1024 * 1024),
        max_files: Some(64),
        max_active_recorders: 8,
    }
}

fn recording_entries(root: &std::path::Path) -> Vec<fs::DirEntry> {
    fs::read_dir(root)
        .expect("recording entries")
        .map(|entry| entry.expect("recording entry"))
        .filter(|entry| entry.file_name() != ".oxiroute-recording.lock")
        .collect()
}

fn recording_entry_names(root: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(root)
        .expect("recording entries")
        .map(|entry| entry.expect("recording entry").file_name())
        .collect();
    names.sort();
    names
}

fn commit_after_barrier(
    recording: oxiroute_rtmp::RecordingFile,
    barrier: Arc<Barrier>,
) -> thread::JoinHandle<oxiroute_rtmp::RecordingCommit> {
    thread::spawn(move || {
        barrier.wait();
        recording.commit().expect("concurrent publish")
    })
}
