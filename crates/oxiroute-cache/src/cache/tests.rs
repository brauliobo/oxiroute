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

fn charged(cache: &Cache) -> (usize, usize) {
    cache
        .shared
        .shards
        .iter()
        .fold((0, 0), |(count, bytes), shard| {
            let state = shard.lock();
            let held = state
                .entries
                .iter()
                .map(|(_, stored)| stored.entry.charge)
                .collect::<Vec<_>>();
            (count + held.len(), bytes + held.iter().sum::<usize>())
        })
}

#[test]
fn a_fresh_hit_counts_only_the_hit_and_shares_the_stored_representation() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    assert!(cache.fresh_hit(request("/a", &empty)).is_none());
    assert_eq!(cache.stats().misses, 0);

    let entry = prepared(&cache, "/a", &empty, &response);
    store_delta(&cache, entry);
    let hit = cache.fresh_hit(request("/a", &empty)).expect("fresh hit");
    assert_eq!(hit.status(), StatusCode::OK);
    assert_eq!(hit.body().as_ref(), b"body");
    assert!(hit.headers().get(http::header::AGE).is_none());
    assert_eq!(cache.stats().hits, 0);
    cache.record_hit(&hit);
    let stats = cache.stats();
    assert_eq!((stats.hits, stats.misses), (1, 0));
    assert!(cache.fresh_hit(request("/b", &empty)).is_none());
    assert_eq!(cache.stats().misses, 0);
}

#[test]
fn a_fresh_hit_leaves_every_request_that_needs_more_to_lookup() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    store_delta(&cache, prepared(&cache, "/a", &empty, &response));
    let mut no_cache = HeaderMap::new();
    no_cache.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    let mut conditional = HeaderMap::new();
    conditional.insert(http::header::IF_NONE_MATCH, HeaderValue::from_static("*"));
    let mut malformed = HeaderMap::new();
    malformed.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=\"x"),
    );
    for headers in [&no_cache, &conditional, &malformed] {
        assert!(cache.fresh_hit(request("/a", headers)).is_none());
    }
    let stats = cache.stats();
    assert_eq!(
        (
            stats.hits,
            stats.misses,
            stats.revalidations,
            stats.bypasses
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        lookup_status(&cache, "/a", &no_cache),
        LookupStatus::Revalidate
    );
    assert_eq!(
        lookup_status(&cache, "/a", &conditional),
        LookupStatus::Bypass
    );
}

#[test]
fn a_fresh_hit_matches_the_vary_of_its_own_request_and_renews_the_entry() {
    let cache = Cache::new(CacheConfig {
        max_entries: 2,
        ..CacheConfig::default()
    })
    .expect("cache");
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
    let (en, fr, empty) = (language("en"), language("fr"), HeaderMap::new());
    store_delta(&cache, prepared(&cache, "/v", &en, &varied));
    store_delta(&cache, prepared(&cache, "/o", &empty, &cacheable()));
    assert!(cache.fresh_hit(request("/v", &fr)).is_none());
    assert!(cache.fresh_hit(request("/v", &empty)).is_none());
    cache.record_hit(&cache.fresh_hit(request("/v", &en)).expect("hit"));
    store_delta(&cache, prepared(&cache, "/p", &empty, &cacheable()));
    assert!(cache.fresh_hit(request("/v", &en)).is_some());
    assert!(cache.fresh_hit(request("/o", &empty)).is_none());
}

#[test]
fn concurrent_stores_and_lookups_keep_the_whole_cache_within_its_bounds() {
    let config = CacheConfig {
        max_entries: 64,
        max_total_bytes: 64 * 1024,
        max_object_bytes: 4 * 1024,
        max_body_bytes: 4 * 1024,
        ..CacheConfig::default()
    };
    let cache = Cache::new(config.clone()).expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let (cache, empty, response) = (&cache, &empty, &response);
            scope.spawn(move || {
                for index in 0..400 {
                    let path = format!("/w{}/{}", worker, index % 97);
                    if index % 3 == 0 {
                        store_delta(cache, prepared(cache, &path, empty, response));
                    }
                    if let Some(hit) = cache.fresh_hit(request(&path, empty)) {
                        cache.record_hit(&hit);
                    }
                    let _ = cache.lookup(request(&path, empty));
                }
            });
        }
    });
    let stats = cache.stats();
    let (held, bytes) = charged(&cache);
    assert_eq!((stats.entries, stats.bytes_used), (held, bytes));
    assert!(held <= config.max_entries && bytes <= config.max_total_bytes);
    assert!(stats.evictions > 0 && stats.stores > 0 && stats.hits > 0);
    assert_eq!(stats.in_flight, 0);
}

#[test]
fn concurrent_hits_of_one_entry_are_all_counted() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    store_delta(&cache, prepared(&cache, "/hot", &empty, &response));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..2_000 {
                    let hit = cache.fresh_hit(request("/hot", &empty)).expect("hit");
                    cache.record_hit(&hit);
                }
            });
        }
    });
    assert_eq!(cache.stats().hits, 16_000);
}

#[test]
fn concurrent_fills_of_one_base_have_one_leader_and_bounded_flights_across_shards() {
    let cache = Cache::new(CacheConfig {
        max_in_flight: 4,
        ..CacheConfig::default()
    })
    .expect("cache");
    let empty = HeaderMap::new();
    let bases = (0..16)
        .map(|index| BaseKey::new(request(&format!("/f{index}"), &empty), 1024).expect("base key"))
        .collect::<Vec<_>>();
    let mut leaders = Vec::new();
    let mut capped = 0;
    for base in &bases {
        match cache.begin_fill(base.clone()).expect("fill") {
            FillJoin::Leader(guard) => leaders.push(guard),
            FillJoin::AtCapacity => capped += 1,
            FillJoin::Follower(_) => panic!("a new base has no leader yet"),
        }
    }
    assert_eq!((leaders.len(), capped), (4, 12));
    assert_eq!(cache.stats().in_flight, 4);
    let followers = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| scope.spawn(|| cache.begin_fill(bases[0].clone()).expect("fill")))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| matches!(handle.join().expect("joined"), FillJoin::Follower(_)))
            .filter(|follower| *follower)
            .count()
    });
    assert_eq!(followers, 8);
    drop(leaders);
    assert_eq!(cache.stats().in_flight, 0);
}

#[test]
fn purging_by_tag_reaches_the_entries_of_every_shard() {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let (empty, response) = (HeaderMap::new(), cacheable());
    let now = cache.now();
    let timing = ResponseTiming {
        request_started: now,
        response_received: now,
        response_received_wall: SystemTime::now(),
    };
    for index in 0..200 {
        let path = format!("/t{index}");
        let tag: &[u8] = if index % 2 == 0 { b"even" } else { b"odd" };
        let entry = cache
            .prepare(
                request(&path, &empty),
                StatusCode::OK,
                &response,
                Bytes::from_static(b"body"),
                timing,
                &[tag],
            )
            .expect("prepared");
        store_delta(&cache, entry);
    }
    let purged = cache.purge_tag(b"even").expect("purged");
    assert_eq!(purged.entries, 100);
    let (held, bytes) = charged(&cache);
    assert_eq!(held, 100);
    assert_eq!(cache.stats().bytes_used, bytes);
    assert_eq!(cache.stats().purged, 100);
    assert!(cache.fresh_hit(request("/t0", &empty)).is_none());
    assert!(cache.fresh_hit(request("/t1", &empty)).is_some());
}
