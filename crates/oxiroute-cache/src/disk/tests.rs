use super::*;

fn test_config() -> DiskCacheConfig {
    let mut config = DiskCacheConfig::default();
    config.memory.max_entries = 4;
    config.memory.max_total_bytes = 32 * 1024;
    config.memory.max_object_bytes = 8 * 1024;
    config.memory.max_body_bytes = 4 * 1024;
    config.max_disk_bytes = 64 * 1024;
    config.max_disk_files = 4;
    config.max_record_bytes = 16 * 1024;
    config
}

fn request(headers: &HeaderMap) -> RequestKeyInput<'_> {
    RequestKeyInput {
        method: &Method::GET,
        scheme: "https",
        authority: "example.com",
        path: "/fault",
        query: None,
        headers,
    }
}

fn attempt_store(cache: &DiskCache, point: FaultPoint) -> Result<StoreOutcome, DiskCacheError> {
    let request_headers = HeaderMap::new();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    let now = cache.now();
    let entry = cache.prepare(
        request(&request_headers),
        StatusCode::OK,
        &response_headers,
        Bytes::from_static(b"fault"),
        ResponseTiming {
            request_started: now,
            response_received: now,
            response_received_wall: SystemTime::now(),
        },
        &[],
    )?;
    cache.inject(point);
    match cache.begin_fill(entry.key().base().clone())? {
        DiskFillJoin::Leader(leader) => leader.store(entry),
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("expected leader"),
    }
}

#[test]
fn validation_does_not_create_or_lease_the_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("cache");

    DiskCache::validate(&root, &test_config()).expect("absent root validation");
    assert!(!root.exists());

    let cache = DiskCache::open(&root, test_config()).expect("active cache");
    DiskCache::validate(&root, &test_config()).expect("active root validation");
    assert!(matches!(
        DiskCache::open(&root, test_config()),
        Err(DiskCacheError::AlreadyOwned)
    ));
    drop(cache);
}

#[test]
fn publication_faults_recover_to_a_complete_record_or_a_miss() {
    for point in [
        FaultPoint::AfterFileSync,
        FaultPoint::AfterPublish,
        FaultPoint::BeforeDirectorySync,
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cache");
        let cache = DiskCache::open(&root, test_config()).expect("cache");
        assert!(matches!(
            attempt_store(&cache, point),
            Err(DiskCacheError::Injected)
        ));
        drop(cache);

        let recovered = DiskCache::open(&root, test_config()).expect("recovery");
        assert!(recovered.stats().disk_entries <= 1);
        let names = std::fs::read_dir(&root)
            .expect("root")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|name| is_temp_name(name.as_bytes())));
        let headers = HeaderMap::new();
        match recovered.lookup(request(&headers)).expect("lookup") {
            Lookup::Hit { response, .. } => {
                assert_eq!(response.body, Bytes::from_static(b"fault"));
            }
            Lookup::Miss { .. } => {}
            other => panic!("unexpected recovered state: {other:?}"),
        }
    }
}

#[test]
fn post_memory_reconciliation_fault_closes_and_recovers_published_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("cache");
    let cache = DiskCache::open(&root, test_config()).expect("cache");
    assert!(matches!(
        attempt_store(&cache, FaultPoint::BeforeMemoryReconciliation),
        Err(DiskCacheError::Injected)
    ));
    assert!(matches!(
        cache.lookup(request(&HeaderMap::new())),
        Err(DiskCacheError::Closed)
    ));
    drop(cache);

    let recovered = DiskCache::open(&root, test_config()).expect("recovery");
    assert!(matches!(
        recovered.lookup(request(&HeaderMap::new())),
        Ok(Lookup::Hit { response, .. })
            if response.body == Bytes::from_static(b"fault")
    ));
}

#[test]
fn insertion_reconciliation_preserves_the_new_record_on_replacement_overlap() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("cache");
    let cache = DiskCache::open(&root, test_config()).expect("cache");
    let request_headers = HeaderMap::new();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    let now = cache.now();
    let entry = cache
        .prepare(
            request(&request_headers),
            StatusCode::OK,
            &response_headers,
            Bytes::from_static(b"record"),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )
        .expect("entry");
    let key = entry.key.clone();
    match cache.begin_fill(key.base().clone()).expect("fill") {
        DiskFillJoin::Leader(leader) => leader.store(entry).expect("store"),
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
    };

    let mut state = cache.shared.lock();
    let delta = InsertionDelta {
        inserted: key.clone(),
        removed: vec![key.clone()],
        evicted: 0,
    };
    assert_eq!(
        reconcile_insertion(&cache.shared.root, &mut state, &delta).expect("reconcile"),
        0
    );
    assert!(state.records.contains_key(&key));
}

#[test]
fn recovered_entries_receive_the_current_cache_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("cache");
    let cache = DiskCache::open(&root, test_config()).expect("cache");
    let request_headers = HeaderMap::new();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    let now = cache.now();
    let entry = cache
        .prepare(
            request(&request_headers),
            StatusCode::OK,
            &response_headers,
            Bytes::from_static(b"recovered"),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )
        .expect("entry");
    let key = entry.key().clone();
    match cache.begin_fill(key.base().clone()).expect("fill") {
        DiskFillJoin::Leader(leader) => {
            leader.store(entry).expect("initial store");
        }
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
    }
    drop(cache);

    let recovered = DiskCache::open(&root, test_config()).expect("recovery");
    let entry = recovered.shared.cache.entry(&key).expect("recovered entry");
    match recovered.begin_fill(key.base().clone()).expect("fill") {
        DiskFillJoin::Leader(leader) => assert!(matches!(
            leader.store(entry),
            Ok(StoreOutcome::Stored { .. })
        )),
        DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
    }
}
