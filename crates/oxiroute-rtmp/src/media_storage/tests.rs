use tempfile::tempdir;

use super::*;
use crate::LiveHub;

fn limits() -> MediaStoreLimits {
    MediaStoreLimits {
        max_bytes: 1024,
        max_files: 8,
        max_active_streams: 2,
        max_file_bytes: 512,
    }
}

#[test]
fn publishes_atomically_and_rejects_stale_incarnations() {
    let root = tempdir().expect("temporary media root");
    let store = MediaStore::open(root.path().join("hls"), limits()).expect("media store");
    let hub = LiveHub::new(crate::LiveHubLimits::default());
    let key = StreamKey::new("service", "application", "stream");
    let first = hub.attach_publisher(key.clone()).expect("first publisher");
    let first_incarnation = first.incarnation();
    let first_prefix = store
        .attach(&key, first_incarnation)
        .expect("first media incarnation");
    store
        .publish(
            &key,
            first_incarnation,
            &first_prefix.join("index.m3u8"),
            b"#EXTM3U",
        )
        .expect("atomic media publish");
    assert_eq!(
        store
            .read_relative(&first_prefix.join("index.m3u8"), 64)
            .expect("published playlist"),
        b"#EXTM3U"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(root.path().join("hls"))
                .expect("media root permissions")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(
                root.path()
                    .join("hls")
                    .join(&first_prefix)
                    .join("index.m3u8")
            )
            .expect("published permissions")
            .permissions()
            .mode()
                & 0o777,
            0o600
        );
    }

    drop(first);
    let second = hub.attach_publisher(key.clone()).expect("second publisher");
    let second_prefix = store
        .attach(&key, second.incarnation())
        .expect("second media incarnation");
    assert_ne!(first_prefix, second_prefix);
    assert!(matches!(
        store.publish(
            &key,
            first_incarnation,
            &second_prefix.join("stale.ts"),
            b"stale",
        ),
        Err(MediaStoreError::StaleIncarnation)
    ));
    assert_eq!(store.stats().files, 0);
    store.close(&key, second.incarnation());
    assert_eq!(store.stats().active_streams, 0);
    assert!(store.current_prefix(&key).is_none());
}

#[test]
fn enforces_file_and_storage_quotas() {
    let root = tempdir().expect("temporary media root");
    let small_limits = MediaStoreLimits {
        max_bytes: 4,
        max_files: 1,
        max_active_streams: 1,
        max_file_bytes: 4,
    };
    let store = MediaStore::open(root.path().join("hls"), small_limits).expect("media store");
    let hub = LiveHub::new(crate::LiveHubLimits::default());
    let key = StreamKey::new("service", "application", "stream");
    let lease = hub.attach_publisher(key.clone()).expect("publisher");
    let prefix = store
        .attach(&key, lease.incarnation())
        .expect("media incarnation");
    assert!(matches!(
        store.publish(
            &key,
            lease.incarnation(),
            &prefix.join("too-large.ts"),
            b"12345"
        ),
        Err(MediaStoreError::FileTooLarge)
    ));
    store
        .publish(&key, lease.incarnation(), &prefix.join("one.ts"), b"1234")
        .expect("first file");
    assert!(matches!(
        store.publish(&key, lease.incarnation(), &prefix.join("two.ts"), b"1"),
        Err(MediaStoreError::Quota)
    ));
}

#[cfg(unix)]
#[test]
fn rejects_publish_after_the_stream_directory_is_replaced_by_a_symlink() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("temporary media root");
    let outside = tempdir().expect("outside media root");
    let store = MediaStore::open(root.path().join("hls"), limits()).expect("media store");
    let hub = LiveHub::new(crate::LiveHubLimits::default());
    let key = StreamKey::new("service", "application", "stream");
    let publisher = hub.attach_publisher(key.clone()).expect("publisher");
    let prefix = store
        .attach(&key, publisher.incarnation())
        .expect("media incarnation");
    let stream_directory = root.path().join("hls").join(&prefix);
    std::fs::remove_dir_all(&stream_directory).expect("replace stream directory");
    symlink(outside.path(), &stream_directory).expect("stream directory symlink");

    assert!(matches!(
        store.publish(
            &key,
            publisher.incarnation(),
            &prefix.join("index.m3u8"),
            b"#EXTM3U",
        ),
        Err(MediaStoreError::InvalidPath)
    ));
    assert!(!outside.path().join("index.m3u8").exists());
}

#[test]
fn continuing_attach_preserves_media_across_publisher_restart() {
    let root = tempdir().expect("temporary media root");
    let store = MediaStore::open(root.path().join("dash"), limits()).expect("media store");
    let hub = LiveHub::new(crate::LiveHubLimits::default());
    let key = StreamKey::new("service", "application", "stream");
    let first = hub.attach_publisher(key.clone()).expect("first publisher");
    let first_incarnation = first.incarnation();
    let first_prefix = store
        .attach_continuing(&key, first_incarnation)
        .expect("first media incarnation");
    store
        .publish(
            &key,
            first_incarnation,
            &first_prefix.join("manifest.mpd"),
            b"manifest",
        )
        .expect("manifest");
    store.close(&key, first_incarnation);
    drop(first);
    drop(store);

    let store = MediaStore::open(root.path().join("dash"), limits()).expect("reopened store");
    let second = hub.attach_publisher(key.clone()).expect("second publisher");
    let second_prefix = store
        .attach_continuing(&key, second.incarnation())
        .expect("continued media incarnation");
    assert_ne!(first_prefix, second_prefix);
    assert_eq!(
        store
            .read_relative(&second_prefix.join("manifest.mpd"), 64)
            .expect("continued manifest"),
        b"manifest"
    );
}
