use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};

use super::*;

const BODY_BYTES: usize = 1000;

fn config(max_total_bytes: usize) -> CacheConfig {
    CacheConfig {
        max_total_bytes,
        max_object_bytes: max_total_bytes,
        max_body_bytes: BODY_BYTES,
        ..CacheConfig::default()
    }
}

fn request(path: &str) -> RequestKeyInput<'_> {
    static EMPTY: std::sync::OnceLock<HeaderMap> = std::sync::OnceLock::new();
    RequestKeyInput {
        method: &Method::GET,
        scheme: "https",
        authority: "example.com",
        path,
        query: None,
        headers: EMPTY.get_or_init(HeaderMap::new),
    }
}

fn prepare(cache: &Cache, path: &str) -> PreparedEntry {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=600"),
    );
    let now = cache.now();
    cache
        .prepare(
            request(path),
            StatusCode::OK,
            &headers,
            Bytes::from(vec![b'a'; BODY_BYTES]),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )
        .expect("prepared entry")
}

fn store(cache: &Cache, entry: PreparedEntry) {
    let FillJoin::Leader(leader) = cache.begin_fill(entry.key.base().clone()).expect("fill") else {
        panic!("expected leader");
    };
    leader.store(entry).expect("stored");
}

fn hit(cache: &Cache, path: &str) -> CachedResponse {
    match cache.lookup(request(path)).expect("lookup") {
        Lookup::Hit { response, .. } => response,
        _ => panic!("expected a hit for {path}"),
    }
}

fn cached(cache: &Cache, path: &str) -> CachedResponse {
    store(cache, prepare(cache, path));
    hit(cache, path)
}

fn coded(bytes: usize) -> Bytes {
    Bytes::from(vec![b'z'; bytes])
}

#[tokio::test]
async fn variant_is_built_once_and_charged_to_the_entry() {
    let cache = Cache::new(config(64 * 1024)).expect("cache");
    let response = cached(&cache, "/page");
    let before = cache.stats().bytes_used;
    let builds = AtomicUsize::new(0);
    let build = |body: Bytes| {
        builds.fetch_add(1, Ordering::SeqCst);
        assert_eq!(body.len(), BODY_BYTES);
        std::future::ready(Some(coded(100)))
    };
    let (first, second) = tokio::join!(
        cache.variant(&response, Coding::Brotli, build),
        cache.variant(&response, Coding::Brotli, build),
    );
    assert_eq!(first.expect("variant").len(), 100);
    assert_eq!(second.expect("variant").len(), 100);
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(cache.stats().bytes_used, before + 100);
    let reused = cache
        .variant(&hit(&cache, "/page"), Coding::Brotli, build)
        .await;
    assert_eq!(reused.expect("variant").len(), 100);
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn each_coding_has_its_own_variant() {
    let cache = Cache::new(config(64 * 1024)).expect("cache");
    let response = cached(&cache, "/page");
    let before = cache.stats().bytes_used;
    for (coding, bytes) in [
        (Coding::Gzip, 300),
        (Coding::Brotli, 200),
        (Coding::Zstd, 250),
    ] {
        let variant = cache
            .variant(&response, coding, |_| {
                std::future::ready(Some(coded(bytes)))
            })
            .await;
        assert_eq!(variant.expect("variant").len(), bytes);
    }
    assert_eq!(cache.stats().bytes_used, before + 750);
}

#[tokio::test]
async fn declined_build_serves_uncoded_without_charge() {
    let cache = Cache::new(config(64 * 1024)).expect("cache");
    let response = cached(&cache, "/page");
    let before = cache.stats().bytes_used;
    let declined = cache
        .variant(&response, Coding::Gzip, |_| std::future::ready(None))
        .await;
    assert!(declined.is_none());
    assert_eq!(cache.stats().bytes_used, before);
}

#[tokio::test]
async fn purging_the_entry_releases_its_variants() {
    let cache = Cache::new(config(64 * 1024)).expect("cache");
    let response = cached(&cache, "/page");
    cache
        .variant(&response, Coding::Zstd, |_| {
            std::future::ready(Some(coded(400)))
        })
        .await
        .expect("variant");
    let charged = cache.stats().bytes_used;
    let purged = cache.purge_base(response.key.base());
    assert_eq!(purged.bytes, charged);
    assert_eq!(cache.stats().bytes_used, 0);
    assert_eq!(cache.stats().entries, 0);
}

#[tokio::test]
async fn variant_of_a_purged_or_replaced_entry_is_not_charged() {
    let cache = Cache::new(config(64 * 1024)).expect("cache");
    let stale = cached(&cache, "/page");
    store(&cache, prepare(&cache, "/page"));
    let before = cache.stats().bytes_used;
    let declined = cache
        .variant(&stale, Coding::Gzip, |_| {
            std::future::ready(Some(coded(100)))
        })
        .await;
    assert!(declined.is_none());
    assert_eq!(cache.stats().bytes_used, before);
}

#[tokio::test]
async fn variant_evicts_the_least_recently_used_entry_to_fit() {
    let entry_charge = {
        let probe = Cache::new(config(64 * 1024)).expect("cache");
        prepare(&probe, "/a").charge
    };
    let cache = Cache::new(config(entry_charge * 2 + 150)).expect("cache");
    cached(&cache, "/a");
    let newer = cached(&cache, "/b");
    let variant = cache
        .variant(&newer, Coding::Gzip, |_| {
            std::future::ready(Some(coded(300)))
        })
        .await;
    assert_eq!(variant.expect("variant").len(), 300);
    let stats = cache.stats();
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.evictions, 1);
    assert_eq!(stats.bytes_used, entry_charge + 300);
    assert!(matches!(
        cache.lookup(request("/a")).expect("lookup"),
        Lookup::Miss { .. }
    ));
}

#[tokio::test]
async fn variant_that_cannot_fit_beside_its_own_entry_is_declined() {
    let entry_charge = {
        let probe = Cache::new(config(64 * 1024)).expect("cache");
        prepare(&probe, "/a").charge
    };
    let cache = Cache::new(config(entry_charge + 100)).expect("cache");
    let response = cached(&cache, "/a");
    let declined = cache
        .variant(&response, Coding::Gzip, |_| {
            std::future::ready(Some(coded(101)))
        })
        .await;
    assert!(declined.is_none());
    let stats = cache.stats();
    assert_eq!((stats.entries, stats.bytes_used), (1, entry_charge));
}

#[tokio::test]
async fn variant_beyond_the_object_bound_is_declined() {
    let entry_charge = {
        let probe = Cache::new(config(64 * 1024)).expect("cache");
        prepare(&probe, "/a").charge
    };
    let bounded = CacheConfig {
        max_object_bytes: entry_charge + 100,
        ..config(64 * 1024)
    };
    let cache = Cache::new(bounded).expect("cache");
    let response = cached(&cache, "/a");
    let declined = cache
        .variant(&response, Coding::Gzip, |_| {
            std::future::ready(Some(coded(101)))
        })
        .await;
    assert!(declined.is_none());
    assert_eq!(cache.stats().bytes_used, entry_charge);
}

#[tokio::test]
async fn variant_attached_at_fill_time_is_served_without_building() {
    let cache = Cache::new(config(64 * 1024)).expect("cache");
    let mut entry = prepare(&cache, "/page");
    let plain = entry.charge;
    assert!(cache.attach_variant(&mut entry, Coding::Zstd, Bytes::from(vec![b'z'; 120])));
    assert!(!cache.attach_variant(&mut entry, Coding::Zstd, Bytes::from(vec![b'z'; 10])));
    assert_eq!(entry.charge, plain + 120);
    store(&cache, entry);
    assert_eq!(cache.stats().bytes_used, plain + 120);
    let response = hit(&cache, "/page");
    let variant = cache
        .variant(&response, Coding::Zstd, |_| async {
            panic!("built a variant")
        })
        .await;
    assert_eq!(variant.expect("variant").len(), 120);
}

#[tokio::test]
async fn attached_variant_beyond_the_object_bound_is_refused() {
    let cache = Cache::new(config(2 * BODY_BYTES)).expect("cache");
    let mut entry = prepare(&cache, "/page");
    let plain = entry.charge;
    assert!(!cache.attach_variant(
        &mut entry,
        Coding::Gzip,
        Bytes::from(vec![b'z'; 2 * BODY_BYTES])
    ));
    assert_eq!(entry.charge, plain);
}
