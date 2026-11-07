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
