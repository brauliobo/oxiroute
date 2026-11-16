use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{Cache, CacheError, FillJoin, Lookup, StoreOutcome};

use crate::common::{ManualClock, cache, config, request, response, store, timing};

fn store_outcome(
    cache: &Cache,
    request_headers: &HeaderMap,
    path: &str,
    response_headers: &HeaderMap,
    body: Bytes,
) -> (oxiroute_cache::CacheKey, StoreOutcome) {
    let entry = cache
        .prepare(
            request(&Method::GET, path, request_headers),
            StatusCode::OK,
            response_headers,
            body,
            timing(0),
            &[],
        )
        .expect("prepared entry");
    let key = entry.key().clone();
    let outcome = match cache.begin_fill(key.base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader.store(entry).expect("store entry"),
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("unexpected fill join"),
    };
    (key, outcome)
}

#[test]
fn lru_eviction_is_deterministic_and_quota_accounting_is_exact() {
    let mut limits = config();
    limits.max_entries = 2;
    let clock = Arc::new(ManualClock::default());
    let cache = Cache::with_clock(limits, clock).expect("cache");
    let request_headers = HeaderMap::new();
    let headers = response("max-age=60");
    store(&cache, &request_headers, "/a", &headers, b"a", 0, &[]);
    store(&cache, &request_headers, "/b", &headers, b"b", 0, &[]);
    cache
        .lookup(request(&Method::GET, "/a", &request_headers))
        .expect("touch a");
    store(&cache, &request_headers, "/c", &headers, b"c", 0, &[]);

    assert!(matches!(
        cache.lookup(request(&Method::GET, "/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/a", &request_headers)),
        Ok(Lookup::Hit { .. })
    ));
    let stats = cache.stats();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.evictions, 1);
    assert!(stats.bytes_used > 0 && stats.bytes_used <= cache.config().max_total_bytes);
}

#[test]
fn replacement_and_conflicting_vary_schema_are_not_capacity_evictions() {
    let (cache, _) = cache();
    let empty = HeaderMap::new();
    let headers = response("max-age=60");

    assert_eq!(
        store_outcome(
            &cache,
            &empty,
            "/replace",
            &headers,
            Bytes::from_static(b"old"),
        )
        .1,
        StoreOutcome::Stored { evicted: 0 }
    );
    assert_eq!(
        store_outcome(
            &cache,
            &empty,
            "/replace",
            &headers,
            Bytes::from_static(b"new"),
        )
        .1,
        StoreOutcome::Stored { evicted: 0 }
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/replace", &empty)),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"new")
    ));

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
        &empty,
        "/vary-schema",
        &headers,
        Bytes::from_static(b"plain"),
    );
    assert_eq!(
        store_outcome(
            &cache,
            &en,
            "/vary-schema",
            &vary_headers,
            Bytes::from_static(b"hello"),
        )
        .1,
        StoreOutcome::Stored { evicted: 0 }
    );
    assert_eq!(
        store_outcome(
            &cache,
            &fr,
            "/vary-schema",
            &vary_headers,
            Bytes::from_static(b"bonjour"),
        )
        .1,
        StoreOutcome::Stored { evicted: 0 }
    );
    assert_eq!(
        store_outcome(
            &cache,
            &empty,
            "/vary-schema",
            &headers,
            Bytes::from_static(b"unified"),
        )
        .1,
        StoreOutcome::Stored { evicted: 0 }
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/vary-schema", &en)),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"unified")
    ));
    assert_eq!(cache.stats().evictions, 0);
}

#[test]
fn count_pressure_evicts_exactly_the_least_recently_used_entry() {
    let mut limits = config();
    limits.max_entries = 2;
    let cache = Cache::with_clock(limits, Arc::new(ManualClock::default())).expect("cache");
    let request_headers = HeaderMap::new();
    let headers = response("max-age=60");
    store_outcome(
        &cache,
        &request_headers,
        "/a",
        &headers,
        Bytes::from_static(b"a"),
    );
    store_outcome(
        &cache,
        &request_headers,
        "/b",
        &headers,
        Bytes::from_static(b"b"),
    );
    cache
        .lookup(request(&Method::GET, "/a", &request_headers))
        .expect("touch a");

    assert_eq!(
        store_outcome(
            &cache,
            &request_headers,
            "/c",
            &headers,
            Bytes::from_static(b"c"),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/a", &request_headers)),
        Ok(Lookup::Hit { .. })
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn byte_pressure_removes_one_or_multiple_victims_and_counts_only_capacity() {
    const LARGE: &[u8] = b"large-body-large-body-large-body-large-body-large-body-large-body";
    let request_headers = HeaderMap::new();
    let headers = response("max-age=60");
    let (probe, _) = cache();
    store_outcome(
        &probe,
        &request_headers,
        "/p",
        &headers,
        Bytes::from_static(b"s"),
    );
    let small_charge = probe.stats().bytes_used;
    store_outcome(
        &probe,
        &request_headers,
        "/q",
        &headers,
        Bytes::from_static(LARGE),
    );
    let large_charge = probe.stats().bytes_used - small_charge;
    assert!(large_charge > small_charge && large_charge <= small_charge * 2);

    let mut one_limit = config();
    one_limit.max_total_bytes = small_charge * 2;
    one_limit.max_object_bytes = one_limit.max_total_bytes;
    one_limit.max_body_bytes = one_limit.max_total_bytes;
    let one = Cache::with_clock(one_limit, Arc::new(ManualClock::default())).expect("one cache");
    for path in ["/a", "/b"] {
        store_outcome(
            &one,
            &request_headers,
            path,
            &headers,
            Bytes::from_static(b"s"),
        );
    }
    assert_eq!(
        store_outcome(
            &one,
            &request_headers,
            "/c",
            &headers,
            Bytes::from_static(b"s"),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );

    let mut multiple_limit = config();
    multiple_limit.max_total_bytes = small_charge * 3;
    multiple_limit.max_object_bytes = multiple_limit.max_total_bytes;
    multiple_limit.max_body_bytes = multiple_limit.max_total_bytes;
    let multiple = Cache::with_clock(multiple_limit, Arc::new(ManualClock::default()))
        .expect("multiple cache");
    for path in ["/a", "/b", "/c"] {
        store_outcome(
            &multiple,
            &request_headers,
            path,
            &headers,
            Bytes::from_static(b"s"),
        );
    }
    assert_eq!(
        store_outcome(
            &multiple,
            &request_headers,
            "/d",
            &headers,
            Bytes::from_static(LARGE),
        )
        .1,
        StoreOutcome::Stored { evicted: 2 }
    );

    let mut replacement_limit = config();
    replacement_limit.max_total_bytes = small_charge * 2;
    replacement_limit.max_object_bytes = replacement_limit.max_total_bytes;
    replacement_limit.max_body_bytes = replacement_limit.max_total_bytes;
    let replacement = Cache::with_clock(replacement_limit, Arc::new(ManualClock::default()))
        .expect("replacement cache");
    for path in ["/a", "/b"] {
        store_outcome(
            &replacement,
            &request_headers,
            path,
            &headers,
            Bytes::from_static(b"s"),
        );
    }
    assert_eq!(
        store_outcome(
            &replacement,
            &request_headers,
            "/a",
            &headers,
            Bytes::from_static(LARGE),
        )
        .1,
        StoreOutcome::Stored { evicted: 1 }
    );
    assert!(matches!(
        replacement.lookup(request(&Method::GET, "/b", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert_eq!(replacement.stats().evictions, 1);
}

#[test]
fn prepared_entries_are_bound_to_shared_cache_identity_before_memory_admission() {
    let mut limits = config();
    limits.max_entries = 1;
    let cache_a =
        Cache::with_clock(limits.clone(), Arc::new(ManualClock::default())).expect("cache a");
    let cache_b = Cache::with_clock(limits, Arc::new(ManualClock::default())).expect("cache b");
    let request_headers = HeaderMap::new();
    let response_headers = response("max-age=60");
    store(
        &cache_a,
        &request_headers,
        "/resident",
        &response_headers,
        b"resident",
        0,
        &[],
    );
    let before = cache_a.stats();

    let foreign = cache_b
        .prepare(
            request(&Method::GET, "/foreign", &request_headers),
            StatusCode::OK,
            &response_headers,
            Bytes::from_static(b"foreign"),
            timing(0),
            &[],
        )
        .expect("foreign entry");
    let leader = match cache_a
        .begin_fill(foreign.key().base().clone())
        .expect("foreign fill")
    {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("foreign leader"),
    };
    assert!(matches!(
        leader.store(foreign),
        Err(CacheError::PreparedEntryOwnerMismatch)
    ));

    let after = cache_a.stats();
    assert_eq!(after.entries, before.entries);
    assert_eq!(after.bytes_used, before.bytes_used);
    assert_eq!(after.stores, before.stores);
    assert_eq!(after.evictions, before.evictions);
    assert_eq!(after.in_flight, 0);
    assert!(matches!(
        cache_a.lookup(request(&Method::GET, "/resident", &request_headers)),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"resident")
    ));
    assert!(matches!(
        cache_a.lookup(request(&Method::GET, "/foreign", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));

    let clone = cache_a.clone();
    let compatible = clone
        .prepare(
            request(&Method::GET, "/clone", &request_headers),
            StatusCode::OK,
            &response_headers,
            Bytes::from_static(b"clone"),
            timing(0),
            &[],
        )
        .expect("clone entry");
    match cache_a
        .begin_fill(compatible.key().base().clone())
        .expect("clone fill")
    {
        FillJoin::Leader(leader) => assert!(matches!(
            leader.store(compatible),
            Ok(StoreOutcome::Stored { .. })
        )),
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("clone leader"),
    }
}

#[test]
fn a_stored_response_keeps_no_buffer_its_header_values_were_read_from() {
    let (cache, _clock) = cache();
    let request_headers = HeaderMap::new();
    // A parsed response's values are slices of the buffer it was read into, which also holds the body and room to spare.
    let mut text = b"public, max-age=60".to_vec();
    text.resize(4096, b'x');
    let buffer = Bytes::from(text);
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_maybe_shared(buffer.slice(0..18)).expect("value"),
    );
    store(
        &cache,
        &request_headers,
        "/asset",
        &headers,
        b"body",
        0,
        &[],
    );

    let Ok(Lookup::Hit { response, .. }) =
        cache.lookup(request(&Method::GET, "/asset", &request_headers))
    else {
        panic!("a stored response is a hit");
    };
    let kept = response
        .headers
        .get(http::header::CACHE_CONTROL)
        .expect("header");
    assert_eq!(kept.as_bytes(), b"public, max-age=60");
    let range = buffer.as_ptr_range();
    assert!(
        !range.contains(&kept.as_bytes().as_ptr()),
        "the cached header value is a slice of the buffer it was read in"
    );
}
