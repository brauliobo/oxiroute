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

fn store_outcome(
    cache: &DiskCache,
    path: &str,
    request_headers: &HeaderMap,
    response_headers: &HeaderMap,
    body: Bytes,
) -> (oxiroute_cache::CacheKey, StoreOutcome) {
    let entry = cache
        .prepare(
            request(path, request_headers),
            StatusCode::OK,
            response_headers,
            body,
            timing(cache),
            &[],
        )
        .expect("prepare persistent entry");
    let key = entry.key().clone();
    let outcome = match cache.begin_fill(key.base().clone()).expect("begin fill") {
        DiskFillJoin::Leader(leader) => leader.store(entry).expect("store persistent entry"),
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("expected leader"),
    };
    (key, outcome)
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
fn store_outcome_counts_each_replaced_or_reconciled_physical_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let cache = DiskCache::open(&root, config()).expect("open cache");
    let request_headers = HeaderMap::new();
    let headers = response();

    assert_eq!(
        store_outcome(
            &cache,
            "/replace",
            &request_headers,
            &headers,
            Bytes::from_static(b"old"),
        )
        .1,
        StoreOutcome::Stored { evicted: 0 }
    );
    assert_eq!(
        store_outcome(
            &cache,
            "/replace",
            &request_headers,
            &headers,
            Bytes::from_static(b"new"),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );
    assert_eq!(record_paths(&root).len(), 1);

    let mut vary_headers = headers.clone();
    vary_headers.insert(
        http::header::VARY,
        HeaderValue::from_static("accept-language"),
    );
    let mut en = HeaderMap::new();
    en.insert(
        http::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en"),
    );
    let mut fr = HeaderMap::new();
    fr.insert(
        http::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("fr"),
    );
    store_outcome(
        &cache,
        "/vary-schema",
        &en,
        &vary_headers,
        Bytes::from_static(b"hello"),
    );
    store_outcome(
        &cache,
        "/vary-schema",
        &fr,
        &vary_headers,
        Bytes::from_static(b"bonjour"),
    );
    assert_eq!(
        store_outcome(
            &cache,
            "/vary-schema",
            &request_headers,
            &headers,
            Bytes::from_static(b"unified"),
        )
        .1,
        StoreOutcome::Stored { evicted: 2 }
    );
    assert_eq!(cache.stats().disk_entries, 2);
}

#[test]
fn disk_pressure_and_memory_reconciliation_report_physical_removals() {
    let request_headers = HeaderMap::new();
    let headers = response();

    let count_temp = tempfile::tempdir().expect("count tempdir");
    let mut count_limits = config();
    count_limits.max_disk_files = 2;
    let count = DiskCache::open(cache_root(&count_temp), count_limits).expect("count cache");
    store_outcome(
        &count,
        "/a",
        &request_headers,
        &headers,
        Bytes::from_static(b"a"),
    );
    store_outcome(
        &count,
        "/b",
        &request_headers,
        &headers,
        Bytes::from_static(b"b"),
    );
    count
        .lookup(request("/a", &request_headers))
        .expect("touch a");
    assert_eq!(
        store_outcome(
            &count,
            "/c",
            &request_headers,
            &headers,
            Bytes::from_static(b"c"),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );
    assert!(matches!(
        count.lookup(request("/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));

    let tight_memory_temp = tempfile::tempdir().expect("tight memory tempdir");
    let mut tight_memory_limits = config();
    tight_memory_limits.memory.max_entries = 1;
    tight_memory_limits.max_disk_files = 8;
    let tight_memory = DiskCache::open(cache_root(&tight_memory_temp), tight_memory_limits)
        .expect("tight memory cache");
    store_outcome(
        &tight_memory,
        "/a",
        &request_headers,
        &headers,
        Bytes::from_static(b"a"),
    );
    assert_eq!(
        store_outcome(
            &tight_memory,
            "/b",
            &request_headers,
            &headers,
            Bytes::from_static(b"b"),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );
    assert_eq!(tight_memory.stats().disk_entries, 1);

    let tight_disk_temp = tempfile::tempdir().expect("tight disk tempdir");
    let mut tight_disk_limits = config();
    tight_disk_limits.memory.max_entries = 8;
    tight_disk_limits.max_disk_files = 1;
    let tight_disk =
        DiskCache::open(cache_root(&tight_disk_temp), tight_disk_limits).expect("tight disk cache");
    store_outcome(
        &tight_disk,
        "/a",
        &request_headers,
        &headers,
        Bytes::from_static(b"a"),
    );
    assert_eq!(
        store_outcome(
            &tight_disk,
            "/b",
            &request_headers,
            &headers,
            Bytes::from_static(b"b"),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );
    assert_eq!(tight_disk.stats().memory.entries, 1);
}

#[test]
fn disk_byte_pressure_removes_multiple_victims_including_replacement() {
    const LARGE: &[u8] = b"large-body-large-body-large-body-large-body-large-body-large-body";
    let request_headers = HeaderMap::new();
    let headers = response();
    let probe_temp = tempfile::tempdir().expect("probe tempdir");
    let probe = DiskCache::open(cache_root(&probe_temp), config()).expect("probe cache");
    store_outcome(
        &probe,
        "/p",
        &request_headers,
        &headers,
        Bytes::from_static(b"s"),
    );
    let small_size = probe.stats().disk_bytes;
    store_outcome(
        &probe,
        "/q",
        &request_headers,
        &headers,
        Bytes::from_static(LARGE),
    );
    let large_size = probe.stats().disk_bytes - small_size;
    assert!(large_size > small_size && large_size <= small_size * 2);

    let multiple_temp = tempfile::tempdir().expect("multiple tempdir");
    let mut multiple_limits = config();
    multiple_limits.max_disk_bytes = small_size * 3;
    multiple_limits.max_record_bytes =
        usize::try_from(multiple_limits.max_disk_bytes).expect("bounded multiple quota");
    let multiple =
        DiskCache::open(cache_root(&multiple_temp), multiple_limits).expect("multiple cache");
    for path in ["/a", "/b", "/c"] {
        store_outcome(
            &multiple,
            path,
            &request_headers,
            &headers,
            Bytes::from_static(b"s"),
        );
    }
    assert_eq!(
        store_outcome(
            &multiple,
            "/d",
            &request_headers,
            &headers,
            Bytes::from_static(LARGE),
        )
        .1,
        StoreOutcome::Stored { evicted: 2 }
    );

    let replacement_temp = tempfile::tempdir().expect("replacement tempdir");
    let mut replacement_limits = config();
    replacement_limits.max_disk_bytes = small_size * 2;
    replacement_limits.max_record_bytes =
        usize::try_from(replacement_limits.max_disk_bytes).expect("bounded replacement quota");
    let replacement = DiskCache::open(cache_root(&replacement_temp), replacement_limits)
        .expect("replacement cache");
    for path in ["/a", "/b"] {
        store_outcome(
            &replacement,
            path,
            &request_headers,
            &headers,
            Bytes::from_static(b"s"),
        );
    }
    assert_eq!(
        store_outcome(
            &replacement,
            "/a",
            &request_headers,
            &headers,
            Bytes::from_static(LARGE),
        )
        .1,
        StoreOutcome::Stored { evicted: 2 }
    );
    assert!(matches!(
        replacement.lookup(request("/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
}

#[test]
fn obsolete_disk_generation_does_not_publish_a_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = cache_root(&temp);
    let cache = DiskCache::open(&root, config()).expect("open cache");
    let request_headers = HeaderMap::new();
    let entry = cache
        .prepare(
            request("/generation", &request_headers),
            StatusCode::OK,
            &response(),
            Bytes::from_static(b"obsolete"),
            timing(&cache),
            &[],
        )
        .expect("prepare entry");
    let leader = match cache
        .begin_fill(entry.key().base().clone())
        .expect("begin fill")
    {
        DiskFillJoin::Leader(leader) => leader,
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("expected leader"),
    };
    assert_eq!(
        cache
            .purge_exact(entry.key())
            .expect("cancel fill")
            .fills_cancelled,
        1
    );
    assert_eq!(
        leader.store(entry).expect("obsolete store"),
        StoreOutcome::GenerationLost
    );
    assert_eq!(cache.stats().disk_entries, 0);
    assert!(record_paths(&root).is_empty());
}

#[test]
fn restart_reconciles_asymmetric_memory_and_disk_limits() {
    let request_headers = HeaderMap::new();

    let tight_memory_temp = tempfile::tempdir().expect("tight memory tempdir");
    let tight_memory_root = cache_root(&tight_memory_temp);
    let seed = DiskCache::open(&tight_memory_root, config()).expect("seed tight memory");
    for path in ["/a", "/b", "/c"] {
        store(&seed, path, path.as_bytes(), &[]);
    }
    seed.lookup(request("/a", &request_headers))
        .expect("touch a");
    drop(seed);
    let mut tighter_memory = config();
    tighter_memory.memory.max_entries = 2;
    tighter_memory.max_disk_files = 8;
    let recovered =
        DiskCache::open(&tight_memory_root, tighter_memory).expect("recover tighter memory");
    let stats = recovered.stats();
    assert_eq!(stats.recovered, 3);
    assert_eq!(stats.memory.entries, 2);
    assert_eq!(stats.disk_entries, 2);
    assert_eq!(record_paths(&tight_memory_root).len(), 2);
    assert!(matches!(
        recovered.lookup(request("/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        recovered.lookup(request("/a", &request_headers)),
        Ok(Lookup::Hit { .. })
    ));

    let tight_disk_temp = tempfile::tempdir().expect("tight disk tempdir");
    let tight_disk_root = cache_root(&tight_disk_temp);
    let seed = DiskCache::open(&tight_disk_root, config()).expect("seed tight disk");
    for path in ["/a", "/b", "/c"] {
        store(&seed, path, path.as_bytes(), &[]);
    }
    seed.lookup(request("/a", &request_headers))
        .expect("touch a");
    drop(seed);
    let mut tighter_disk = config();
    tighter_disk.memory.max_entries = 8;
    tighter_disk.max_disk_files = 2;
    let recovered = DiskCache::open(&tight_disk_root, tighter_disk).expect("recover tighter disk");
    let stats = recovered.stats();
    assert_eq!(stats.recovered, 2);
    assert_eq!(stats.memory.entries, 2);
    assert_eq!(stats.disk_entries, 2);
    assert_eq!(stats.corrupt_records_removed, 1);
    assert_eq!(record_paths(&tight_disk_root).len(), 2);
    assert!(matches!(
        recovered.lookup(request("/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        recovered.lookup(request("/a", &request_headers)),
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
