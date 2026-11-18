use std::time::SystemTime;

use super::*;

fn cacheable() -> HeaderMap {
    let mut response = HeaderMap::new();
    response.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    response
}

fn store(cache: &Cache, path: &str) {
    let now = cache.now();
    let entry = cache
        .prepare(
            RequestKeyInput {
                method: &Method::GET,
                scheme: "https",
                authority: "example.com",
                path,
                query: None,
                headers: &HeaderMap::new(),
            },
            StatusCode::OK,
            &cacheable(),
            Bytes::from_static(b"body"),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )
        .expect("prepared entry");
    let FillJoin::Leader(leader) = cache.begin_fill(entry.key.base().clone()).expect("fill") else {
        panic!("expected leader");
    };
    leader.store(entry).expect("stored");
}

fn status(cache: &Cache, path: &str) -> LookupStatus {
    let lookup = cache
        .lookup(RequestKeyInput {
            method: &Method::GET,
            scheme: "https",
            authority: "example.com",
            path,
            query: None,
            headers: &HeaderMap::new(),
        })
        .expect("lookup");
    match lookup {
        Lookup::Bypass { status }
        | Lookup::Miss { status, .. }
        | Lookup::Hit { status, .. }
        | Lookup::Revalidate { status, .. } => status,
    }
}

fn filled(config: CacheConfig, clock: &Arc<SystemClock>, paths: &[&str]) -> Cache {
    let cache = Cache::with_clock(config, clock.clone()).expect("cache");
    for path in paths {
        store(&cache, path);
    }
    cache
}

#[test]
fn an_inheriting_cache_serves_what_the_replaced_cache_held() {
    let clock = Arc::new(SystemClock::new());
    let old = filled(CacheConfig::default(), &clock, &["/a", "/b", "/c"]);
    let new = Cache::with_clock(CacheConfig::default(), clock).expect("cache");

    let inherited = new.inherit(&old).expect("inherited");

    assert_eq!((inherited.offered, inherited.kept), (3, 3));
    for path in ["/a", "/b", "/c"] {
        assert_eq!(status(&new, path), LookupStatus::Hit);
    }
    assert_eq!(new.stats().bytes_used, old.stats().bytes_used);
}

#[test]
fn the_replaced_cache_keeps_serving_after_it_was_inherited() {
    let clock = Arc::new(SystemClock::new());
    let old = filled(CacheConfig::default(), &clock, &["/a"]);
    let new = Cache::with_clock(CacheConfig::default(), clock).expect("cache");

    new.inherit(&old).expect("inherited");

    assert_eq!(status(&old, "/a"), LookupStatus::Hit);
}

#[test]
fn a_smaller_bound_keeps_the_most_recently_used_representations() {
    let clock = Arc::new(SystemClock::new());
    let old = filled(CacheConfig::default(), &clock, &["/a", "/b", "/c"]);
    assert_eq!(status(&old, "/a"), LookupStatus::Hit);
    let smaller = CacheConfig {
        max_entries: 2,
        ..CacheConfig::default()
    };
    let new = Cache::with_clock(smaller, clock).expect("cache");

    let inherited = new.inherit(&old).expect("inherited");

    assert_eq!((inherited.offered, inherited.kept), (3, 2));
    assert_eq!(status(&new, "/b"), LookupStatus::Miss);
    assert_eq!(status(&new, "/a"), LookupStatus::Hit);
    assert_eq!(status(&new, "/c"), LookupStatus::Hit);
}

#[test]
fn a_representation_over_the_new_object_bound_is_left_out() {
    let clock = Arc::new(SystemClock::new());
    let old = filled(CacheConfig::default(), &clock, &["/a"]);
    let tighter = CacheConfig {
        max_object_bytes: old.stats().bytes_used - 1,
        max_body_bytes: 4,
        ..CacheConfig::default()
    };
    let new = Cache::with_clock(tighter, clock).expect("cache");

    let inherited = new.inherit(&old).expect("inherited");

    assert_eq!((inherited.offered, inherited.kept), (1, 0));
    assert_eq!(new.stats().bytes_used, 0);
}

#[test]
fn caches_on_different_clocks_cannot_hand_over_entries() {
    let old = filled(
        CacheConfig::default(),
        &Arc::new(SystemClock::new()),
        &["/a"],
    );
    let new = Cache::new(CacheConfig::default()).expect("cache");

    assert!(matches!(new.inherit(&old), Err(CacheError::ClockMismatch)));
}

#[test]
fn a_cache_inherits_itself_unchanged() {
    let clock = Arc::new(SystemClock::new());
    let cache = filled(CacheConfig::default(), &clock, &["/a"]);

    let inherited = cache.inherit(&cache.clone()).expect("inherited");

    assert_eq!((inherited.offered, inherited.kept), (1, 1));
}
