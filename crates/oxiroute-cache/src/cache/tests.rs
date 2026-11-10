use std::time::SystemTime;

use super::*;

fn prepared(
    cache: &Cache,
    path: &str,
    request_headers: &HeaderMap,
    response_headers: &HeaderMap,
) -> PreparedEntry {
    let now = cache.now();
    cache
        .prepare(
            RequestKeyInput {
                method: &Method::GET,
                scheme: "https",
                authority: "example.com",
                path,
                query: None,
                headers: request_headers,
            },
            StatusCode::OK,
            response_headers,
            Bytes::from_static(b"body"),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )
        .expect("prepared entry")
}

fn store_delta(cache: &Cache, entry: PreparedEntry) -> InsertionDelta {
    let FillJoin::Leader(leader) = cache.begin_fill(entry.key.base().clone()).expect("fill") else {
        panic!("expected leader");
    };
    let claimed = leader.claim(entry).expect("claimed entry");
    let ClaimedStoreOutcome::Stored(delta) = leader.store_claimed(claimed) else {
        panic!("expected stored delta");
    };
    delta
}

#[test]
fn insertion_delta_reports_replacement_conflicts_and_capacity_in_order() {
    let config = CacheConfig {
        max_entries: 8,
        ..CacheConfig::default()
    };
    let cache = Cache::new(config).expect("cache");
    let empty = HeaderMap::new();
    let mut response = HeaderMap::new();
    response.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );

    let original = prepared(&cache, "/replace", &empty, &response);
    let original_key = original.key.clone();
    assert!(store_delta(&cache, original).removed.is_empty());
    let replacement = store_delta(&cache, prepared(&cache, "/replace", &empty, &response));
    assert_eq!(replacement.inserted, original_key);
    assert_eq!(replacement.removed, vec![replacement.inserted.clone()]);
    assert_eq!(replacement.evicted, 0);

    let mut varied_response = response.clone();
    varied_response.insert(
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
    let en_entry = prepared(&cache, "/vary", &en, &varied_response);
    let en_key = en_entry.key.clone();
    store_delta(&cache, en_entry);
    let fr_entry = prepared(&cache, "/vary", &fr, &varied_response);
    let fr_key = fr_entry.key.clone();
    store_delta(&cache, fr_entry);
    let unified = store_delta(&cache, prepared(&cache, "/vary", &empty, &response));
    let mut expected = vec![en_key, fr_key];
    expected.sort_unstable();
    assert_eq!(unified.removed, expected);
    assert_eq!(unified.evicted, 0);

    let capacity = CacheConfig {
        max_entries: 2,
        ..CacheConfig::default()
    };
    let cache = Cache::new(capacity).expect("capacity cache");
    let first = prepared(&cache, "/a", &empty, &response);
    let first_key = first.key.clone();
    store_delta(&cache, first);
    store_delta(&cache, prepared(&cache, "/b", &empty, &response));
    let pressure = store_delta(&cache, prepared(&cache, "/c", &empty, &response));
    assert_eq!(pressure.removed, vec![first_key]);
    assert_eq!(pressure.evicted, 1);
}

fn cacheable() -> HeaderMap {
    let mut response = HeaderMap::new();
    response.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    response
}

fn lookup_status(cache: &Cache, path: &str, headers: &HeaderMap) -> LookupStatus {
    let lookup = cache
        .lookup(RequestKeyInput {
            method: &Method::GET,
            scheme: "https",
            authority: "example.com",
            path,
            query: None,
            headers,
        })
        .expect("lookup");
    match lookup {
        Lookup::Bypass { status }
        | Lookup::Miss { status, .. }
        | Lookup::Hit { status, .. }
        | Lookup::Revalidate { status, .. } => status,
    }
}

#[test]
fn eviction_takes_the_least_recently_used_entry_and_a_lookup_renews_an_entry() {
    let cache = Cache::new(CacheConfig {
        max_entries: 3,
        ..CacheConfig::default()
    })
    .expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    let mut keys = Vec::new();
    for path in ["/a", "/b", "/c"] {
        let entry = prepared(&cache, path, &empty, &response);
        keys.push(entry.key.clone());
        store_delta(&cache, entry);
    }
    assert_eq!(lookup_status(&cache, "/a", &empty), LookupStatus::Hit);

    let first = store_delta(&cache, prepared(&cache, "/d", &empty, &response));
    assert_eq!(first.removed, vec![keys[1].clone()]);
    let second = store_delta(&cache, prepared(&cache, "/e", &empty, &response));
    assert_eq!(second.removed, vec![keys[2].clone()]);
    assert_eq!(lookup_status(&cache, "/a", &empty), LookupStatus::Hit);
    assert_eq!(lookup_status(&cache, "/b", &empty), LookupStatus::Miss);
    assert_eq!(cache.stats().entries, 3);
}

#[test]
fn many_entries_are_found_and_evicted_in_order_of_use() {
    let cache = Cache::new(CacheConfig {
        max_entries: 100,
        ..CacheConfig::default()
    })
    .expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    for index in 0..250 {
        let path = format!("/p{index}");
        let delta = store_delta(&cache, prepared(&cache, &path, &empty, &response));
        assert_eq!(delta.evicted, usize::from(index >= 100));
        if index >= 100 {
            assert_eq!(delta.removed[0].base().path, format!("/p{}", index - 100));
        }
    }
    assert_eq!(cache.stats().entries, 100);
    assert_eq!(lookup_status(&cache, "/p149", &empty), LookupStatus::Miss);
    assert_eq!(lookup_status(&cache, "/p150", &empty), LookupStatus::Hit);
    assert_eq!(lookup_status(&cache, "/p249", &empty), LookupStatus::Hit);
}

#[test]
fn a_lookup_picks_the_variant_of_its_own_request_among_many_entries() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let empty = HeaderMap::new();
    let mut varied = cacheable();
    varied.insert(
        http::header::VARY,
        HeaderValue::from_static("accept-language"),
    );
    let language = |value: &'static str| {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static(value),
        );
        headers
    };
    for index in 0..300 {
        store_delta(
            &cache,
            prepared(&cache, &format!("/n{index}"), &empty, &cacheable()),
        );
    }
    for tag in ["en", "fr", "de"] {
        store_delta(&cache, prepared(&cache, "/vary", &language(tag), &varied));
    }
    for tag in ["en", "fr", "de"] {
        assert_eq!(
            lookup_status(&cache, "/vary", &language(tag)),
            LookupStatus::Hit
        );
    }
    assert_eq!(
        lookup_status(&cache, "/vary", &language("it")),
        LookupStatus::Miss
    );
    assert_eq!(lookup_status(&cache, "/n299", &empty), LookupStatus::Hit);
}

#[test]
fn purging_a_base_removes_all_its_variants_and_nothing_else() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let empty = HeaderMap::new();
    let mut varied = cacheable();
    varied.insert(
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
    let first = prepared(&cache, "/vary", &en, &varied);
    let base = first.key.base().clone();
    store_delta(&cache, first);
    store_delta(&cache, prepared(&cache, "/vary", &fr, &varied));
    store_delta(&cache, prepared(&cache, "/other", &empty, &cacheable()));

    let purged = cache.purge_base(&base);
    assert_eq!(purged.entries, 2);
    assert_eq!(cache.stats().entries, 1);
    assert_eq!(lookup_status(&cache, "/vary", &en), LookupStatus::Miss);
    assert_eq!(lookup_status(&cache, "/other", &empty), LookupStatus::Hit);
    assert_eq!(cache.purge_base(&base).entries, 0);
}

#[test]
fn entries_renumbered_after_the_sequence_runs_out_keep_their_order_of_use() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    let mut entries = Entries::default();
    let mut keys = Vec::new();
    for (access, path) in [(10, "/a"), (20, "/b"), (30, "/c")] {
        let entry = prepared(&cache, path, &empty, &response);
        keys.push(entry.key.clone());
        entries.insert(entry, access);
    }
    entries.touch(&keys[0], 40);
    assert_eq!(entries.least_recently_used(), Some(&keys[1]));
    assert_eq!(entries.renumber(), 3);
    assert_eq!(
        entries.get(&keys[1]).map(|stored| stored.last_access),
        Some(1)
    );
    assert_eq!(
        entries.get(&keys[0]).map(|stored| stored.last_access),
        Some(3)
    );
    entries.remove(&keys[1]);
    assert_eq!(entries.least_recently_used(), Some(&keys[2]));
    assert_eq!(entries.variants(keys[2].base()), &[keys[2].clone()]);
    assert!(entries.variants(keys[1].base()).is_empty());
}
