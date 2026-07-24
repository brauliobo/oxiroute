use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::SystemTime,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    BaseKey, CacheError, DiskCache, DiskCacheConfig, DiskCacheError, DiskFillJoin, DiskQuotaScope,
    Lookup, RequestKeyInput, ResponseTiming, StoreOutcome,
};

fn config() -> DiskCacheConfig {
    let mut config = DiskCacheConfig::default();
    config.memory.max_entries = 8;
    config.memory.max_total_bytes = 64 * 1024;
    config.memory.max_object_bytes = 8 * 1024;
    config.memory.max_header_bytes = 2 * 1024;
    config.memory.max_header_fields = 32;
    config.memory.max_body_bytes = 4 * 1024;
    config.memory.max_key_bytes = 1024;
    config.memory.max_vary_fields = 8;
    config.memory.max_tags_per_entry = 4;
    config.memory.max_tag_bytes = 32;
    config.memory.max_in_flight = 32;
    config.memory.max_followers_per_fill = 32;
    config.max_disk_bytes = 128 * 1024;
    config.max_disk_files = 8;
    config.max_record_bytes = 16 * 1024;
    config
}

fn request<'a>(path: &'a str, headers: &'a HeaderMap) -> RequestKeyInput<'a> {
    RequestKeyInput {
        method: &Method::GET,
        scheme: "https",
        authority: "example.com",
        path,
        query: None,
        headers,
    }
}

fn response() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    headers
}

fn timing(cache: &DiskCache) -> ResponseTiming {
    let now = cache.now();
    ResponseTiming {
        request_started: now,
        response_received: now,
        response_received_wall: SystemTime::now(),
    }
}

fn store(
    cache: &DiskCache,
    path: &str,
    body: &'static [u8],
    tags: &[&[u8]],
) -> oxiroute_cache::CacheKey {
    let request_headers = HeaderMap::new();
    let entry = cache
        .prepare(
            request(path, &request_headers),
            StatusCode::OK,
            &response(),
            Bytes::from_static(body),
            timing(cache),
            tags,
        )
        .expect("prepare persistent entry");
    let key = entry.key().clone();
    match cache.begin_fill(key.base().clone()).expect("begin fill") {
        DiskFillJoin::Leader(leader) => assert!(matches!(
            leader.store(entry).expect("store persistent entry"),
            StoreOutcome::Stored { .. }
        )),
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("expected leader"),
    }
    key
}

fn cache_root(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("cache")
}

fn record_paths(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .expect("read cache root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".record"))
        })
        .collect()
}

#[test]
fn restart_recovers_records_age_and_tag_index_and_purge_is_durable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let cache = DiskCache::open(&root, config()).expect("open cache");
    assert_eq!(cache.quota_scope(), DiskQuotaScope::ExclusiveRoot);
    let exact = store(&cache, "/exact", b"exact", &[b"group"]);
    store(&cache, "/tagged", b"tagged", &[b"group"]);
    assert_eq!(cache.stats().disk_entries, 2);
    drop(cache);

    let cache = DiskCache::open(&root, config()).expect("recover cache");
    assert_eq!(cache.stats().recovered, 2);
    let headers = HeaderMap::new();
    assert!(matches!(
        cache.lookup(request("/exact", &headers)),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"exact")
    ));
    assert_eq!(cache.purge_exact(&exact).expect("exact purge").entries, 1);
    assert_eq!(cache.purge_tag(b"group").expect("tag purge").entries, 1);
    drop(cache);

    let cache = DiskCache::open(&root, config()).expect("recover purges");
    assert_eq!(cache.stats().disk_entries, 0);
    assert!(matches!(
        cache.lookup(request("/exact", &headers)),
        Ok(Lookup::Miss { .. })
    ));
}

#[test]
fn quotas_are_exact_and_lru_eviction_survives_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let mut limits = config();
    limits.max_disk_files = 2;
    limits.memory.max_entries = 2;
    let cache = DiskCache::open(&root, limits.clone()).expect("open cache");
    store(&cache, "/a", b"a", &[]);
    store(&cache, "/b", b"bb", &[]);
    let headers = HeaderMap::new();
    cache.lookup(request("/a", &headers)).expect("touch a");
    store(&cache, "/c", b"ccc", &[]);
    let stats = cache.stats();
    let disk_bytes = record_paths(&root)
        .iter()
        .map(|path| fs::metadata(path).expect("record metadata").len())
        .sum::<u64>();
    assert_eq!(stats.disk_entries, 2);
    assert_eq!(stats.disk_bytes, disk_bytes);
    assert!(stats.disk_bytes <= limits.max_disk_bytes);
    drop(cache);

    let cache = DiskCache::open(&root, limits).expect("restart cache");
    assert!(matches!(
        cache.lookup(request("/b", &headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        cache.lookup(request("/a", &headers)),
        Ok(Lookup::Hit { .. })
    ));
    assert!(matches!(
        cache.lookup(request("/c", &headers)),
        Ok(Lookup::Hit { .. })
    ));
}

#[test]
fn byte_quota_admits_only_exact_committed_record_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let initial = DiskCache::open(&root, config()).expect("open cache");
    store(&initial, "/a", b"same-size-body", &[]);
    let exact_size = initial.stats().disk_bytes;
    drop(initial);

    let mut limits = config();
    limits.max_disk_bytes = exact_size;
    limits.max_record_bytes = usize::try_from(exact_size).expect("record size");
    let cache = DiskCache::open(&root, limits).expect("reopen exact quota");
    store(&cache, "/b", b"same-size-body", &[]);
    let headers = HeaderMap::new();
    assert_eq!(cache.stats().disk_entries, 1);
    assert_eq!(cache.stats().disk_bytes, exact_size);
    assert!(matches!(
        cache.lookup(request("/a", &headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        cache.lookup(request("/b", &headers)),
        Ok(Lookup::Hit { .. })
    ));
}

#[test]
fn prepared_entries_are_bound_to_shared_cache_identity_before_disk_admission() {
    let temp_a = tempfile::tempdir().expect("tempdir a");
    let temp_b = tempfile::tempdir().expect("tempdir b");
    let root_a = cache_root(&temp_a);
    let root_b = cache_root(&temp_b);
    let mut limits = config();
    limits.memory.max_entries = 1;
    limits.max_disk_files = 1;
    let cache_a = DiskCache::open(&root_a, limits.clone()).expect("cache a");
    let cache_b = DiskCache::open(&root_b, limits).expect("cache b");
    store(&cache_a, "/resident", b"resident", &[]);
    let before = cache_a.stats();
    let mut records_before = record_paths(&root_a);
    records_before.sort();

    let request_headers = HeaderMap::new();
    let foreign = cache_b
        .prepare(
            request("/foreign", &request_headers),
            StatusCode::OK,
            &response(),
            Bytes::from_static(b"foreign"),
            timing(&cache_b),
            &[],
        )
        .expect("foreign entry");
    let leader = match cache_a
        .begin_fill(foreign.key().base().clone())
        .expect("foreign fill")
    {
        DiskFillJoin::Leader(leader) => leader,
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("foreign leader"),
    };
    assert!(matches!(
        leader.store(foreign),
        Err(DiskCacheError::Cache(
            CacheError::PreparedEntryOwnerMismatch
        ))
    ));

    let after = cache_a.stats();
    assert_eq!(after.disk_entries, before.disk_entries);
    assert_eq!(after.disk_bytes, before.disk_bytes);
    assert_eq!(after.memory.entries, before.memory.entries);
    assert_eq!(after.memory.bytes_used, before.memory.bytes_used);
    assert_eq!(after.memory.stores, before.memory.stores);
    assert_eq!(after.memory.evictions, before.memory.evictions);
    assert_eq!(after.memory.in_flight, 0);
    let mut records_after = record_paths(&root_a);
    records_after.sort();
    assert_eq!(records_after, records_before);
    assert!(matches!(
        cache_a.lookup(request("/resident", &request_headers)),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"resident")
    ));
    assert!(matches!(
        cache_a.lookup(request("/foreign", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));

    let clone = cache_a.clone();
    let compatible = clone
        .prepare(
            request("/clone", &request_headers),
            StatusCode::OK,
            &response(),
            Bytes::from_static(b"clone"),
            timing(&clone),
            &[],
        )
        .expect("clone entry");
    match cache_a
        .begin_fill(compatible.key().base().clone())
        .expect("clone fill")
    {
        DiskFillJoin::Leader(leader) => assert!(matches!(
            leader.store(compatible),
            Ok(StoreOutcome::Stored { .. })
        )),
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("clone leader"),
    }
}

#[test]
fn torn_corrupt_and_stale_temp_records_are_removed_on_startup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let cache = DiskCache::open(&root, config()).expect("open cache");
    store(&cache, "/torn", b"body", &[]);
    store(&cache, "/corrupt", b"body", &[]);
    drop(cache);

    let mut records = record_paths(&root);
    records.sort();
    let torn = records.pop().expect("torn record");
    let corrupt = records.pop().expect("corrupt record");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&torn)
        .expect("open record");
    file.set_len(11).expect("tear record");
    let mut corrupt_bytes = fs::read(&corrupt).expect("read corrupt record");
    let last = corrupt_bytes.last_mut().expect("record bytes");
    *last ^= 0xff;
    fs::write(&corrupt, corrupt_bytes).expect("corrupt checksum");
    let temp_name = root.join(format!(".oxiroute-cache-{}.tmp", "a".repeat(32)));
    fs::write(&temp_name, b"partial").expect("write stale temp");
    fs::set_permissions(&temp_name, fs::Permissions::from_mode(0o600)).expect("temp mode");

    let cache = DiskCache::open(&root, config()).expect("recover cache");
    let stats = cache.stats();
    assert_eq!(stats.disk_entries, 0);
    assert_eq!(stats.corrupt_records_removed, 2);
    assert_eq!(stats.stale_temps_removed, 1);
    assert!(!torn.exists());
    assert!(!corrupt.exists());
    assert!(!temp_name.exists());
}

#[test]
fn ownership_symlink_hardlink_and_substitution_attacks_fail_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let cache = DiskCache::open(&root, config()).expect("open cache");
    assert!(matches!(
        DiskCache::open(&root, config()),
        Err(DiskCacheError::AlreadyOwned)
    ));
    let key = store(&cache, "/race", b"safe", &[]);
    let record = record_paths(&root).pop().expect("record");
    let displaced = root.join("displaced");
    fs::rename(&record, &displaced).expect("displace record");
    let victim = temp.path().join("victim");
    fs::write(&victim, b"keep").expect("victim");
    std::os::unix::fs::symlink(&victim, &record).expect("substitute symlink");
    assert!(matches!(
        cache.purge_exact(&key),
        Err(DiskCacheError::UnsafeEntry)
    ));
    assert_eq!(fs::read(&victim).expect("victim intact"), b"keep");
    drop(cache);

    fs::remove_file(&record).expect("remove substitution");
    fs::rename(&displaced, &record).expect("restore record name");
    let external = temp.path().join("external-link");
    fs::hard_link(&record, &external).expect("hard link record");
    let cache = DiskCache::open(&root, config()).expect("remove hardlinked cache record");
    assert_eq!(cache.stats().disk_entries, 0);
    assert!(external.exists());

    let symlink_root = temp.path().join("cache-link");
    std::os::unix::fs::symlink(&root, &symlink_root).expect("root symlink");
    assert!(matches!(
        DiskCache::open(&symlink_root, config()),
        Err(DiskCacheError::RootOpen(_))
    ));
    assert_eq!(
        fs::metadata(&root).expect("root metadata").mode() & 0o777,
        0o700
    );
    assert!(fs::metadata(&external).expect("external metadata").nlink() >= 1);
}

#[test]
fn concurrent_readers_and_independent_fills_remain_consistent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let cache = Arc::new(DiskCache::open(&root, config()).expect("open cache"));
    store(&cache, "/shared", b"shared", &[]);

    let readers = (0..8)
        .map(|_| {
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                let headers = HeaderMap::new();
                for _ in 0..8 {
                    assert!(matches!(
                        cache.lookup(request("/shared", &headers)),
                        Ok(Lookup::Hit { .. })
                    ));
                }
            })
        })
        .collect::<Vec<_>>();
    for reader in readers {
        reader.join().expect("reader");
    }

    let writers = (0..6)
        .map(|index| {
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                let path = format!("/fill-{index}");
                let headers = HeaderMap::new();
                let entry = cache
                    .prepare(
                        request(&path, &headers),
                        StatusCode::OK,
                        &response(),
                        Bytes::from(vec![u8::try_from(index).expect("bounded index"); 16]),
                        timing(&cache),
                        &[],
                    )
                    .expect("prepare fill");
                let base = BaseKey::new(request(&path, &headers), 1024).expect("base");
                match cache.begin_fill(base).expect("begin fill") {
                    DiskFillJoin::Leader(leader) => {
                        leader.store(entry).expect("store fill");
                    }
                    DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
                }
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().expect("writer");
    }
    assert_eq!(cache.stats().disk_entries, 7);
    cache.shutdown().expect("bounded shutdown");
    assert!(matches!(
        cache.lookup(request("/shared", &HeaderMap::new())),
        Err(DiskCacheError::Closed)
    ));
}
