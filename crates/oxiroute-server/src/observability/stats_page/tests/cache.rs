use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::fixture::fixture;
use crate::observability::stats_page::{
    builder::{BuildError, build_snapshot},
    cache::SnapshotCache,
};

#[tokio::test(start_paused = true)]
async fn concurrent_readers_share_one_build_until_the_snapshot_expires() {
    let fixture = fixture();
    let cache = SnapshotCache::new(Duration::from_secs(1));
    let builds = AtomicUsize::new(0);
    let build = || {
        builds.fetch_add(1, Ordering::SeqCst);
        build_snapshot(&fixture.generations, 1_000)
    };

    let entries = futures_util::future::join_all((0..32).map(|_| cache.get(&build))).await;
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    let first = entries[0].as_ref().expect("entry");
    assert!(
        entries
            .iter()
            .all(|entry| Arc::ptr_eq(entry.as_ref().unwrap(), first))
    );

    tokio::time::advance(Duration::from_millis(999)).await;
    assert!(Arc::ptr_eq(&cache.get(&build).await.unwrap(), first));
    assert_eq!(builds.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(2)).await;
    let second = cache.get(&build).await.unwrap();
    assert!(!Arc::ptr_eq(&second, first));
    assert_eq!(builds.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn invalidation_forces_the_next_read_to_rebuild() {
    let fixture = fixture();
    let cache = SnapshotCache::new(Duration::from_secs(1));
    let builds = AtomicUsize::new(0);
    let build = || {
        builds.fetch_add(1, Ordering::SeqCst);
        build_snapshot(&fixture.generations, 1_000)
    };
    cache.get(&build).await.unwrap();
    cache.invalidate();
    cache.get(&build).await.unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_failed_build_is_reported_and_not_kept() {
    let fixture = fixture();
    let cache = SnapshotCache::new(Duration::from_secs(1));
    let failure = cache.get(|| Err(BuildError::NoActiveGeneration)).await;
    assert!(matches!(failure, Err(BuildError::NoActiveGeneration)));
    assert!(
        cache
            .get(|| build_snapshot(&fixture.generations, 1_000))
            .await
            .is_ok()
    );
}

#[tokio::test(start_paused = true)]
async fn each_rendering_is_produced_once_per_snapshot() {
    use crate::observability::stats_page::cache::Rendering;
    let fixture = fixture();
    let cache = SnapshotCache::new(Duration::from_secs(1));
    let entry = cache
        .get(|| build_snapshot(&fixture.generations, 1_000))
        .await
        .unwrap();
    for rendering in [
        Rendering::Json { admin: false },
        Rendering::Json { admin: true },
        Rendering::Csv,
    ] {
        let first = entry.body(rendering, 1_000, "/s");
        assert!(
            std::ptr::eq(first, entry.body(rendering, 1_000, "/s")),
            "{rendering:?}"
        );
    }
    assert!(!std::ptr::eq(
        entry.body(Rendering::Json { admin: false }, 1_000, "/s"),
        entry.body(Rendering::Json { admin: true }, 1_000, "/s"),
    ));
}
