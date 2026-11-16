use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    Cache, CacheConfig, Clock, FillJoin, MonoTime, RequestKeyInput, ResponseTiming, StoreOutcome,
};

#[derive(Default)]
pub(super) struct ManualClock(AtomicU64);

impl ManualClock {
    pub(super) fn set(&self, seconds: u64) {
        self.0.store(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> MonoTime {
        MonoTime::from_duration(Duration::from_secs(self.0.load(Ordering::SeqCst)))
    }
}

pub(super) fn config() -> CacheConfig {
    CacheConfig {
        max_entries: 8,
        max_total_bytes: 32 * 1024,
        max_object_bytes: 8 * 1024,
        max_header_bytes: 2 * 1024,
        max_header_fields: 32,
        max_body_bytes: 4 * 1024,
        max_key_bytes: 1024,
        max_vary_fields: 8,
        max_tags_per_entry: 4,
        max_tag_bytes: 32,
        max_in_flight: 8,
        max_followers_per_fill: 64,
        max_heuristic_freshness: Duration::from_hours(24),
    }
}

pub(super) fn cache() -> (Cache, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::default());
    let cache = Cache::with_clock(config(), clock.clone()).expect("valid cache");
    (cache, clock)
}

pub(super) fn request<'a>(
    method: &'a Method,
    path: &'a str,
    headers: &'a HeaderMap,
) -> RequestKeyInput<'a> {
    RequestKeyInput {
        method,
        scheme: "HTTPS",
        authority: "EXAMPLE.COM:443",
        path,
        query: Some("a=1&b=2"),
        headers,
    }
}

pub(super) fn timing(received: u64) -> ResponseTiming {
    ResponseTiming {
        request_started: MonoTime::from_duration(Duration::from_secs(received.saturating_sub(2))),
        response_received: MonoTime::from_duration(Duration::from_secs(received)),
        response_received_wall: SystemTime::UNIX_EPOCH
            + Duration::from_secs(784_111_777 + received),
    }
}

pub(super) fn response(cache_control: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_str(cache_control).expect("cache control"),
    );
    headers.insert(
        http::header::DATE,
        HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
    );
    headers
}

pub(super) fn store(
    cache: &Cache,
    request_headers: &HeaderMap,
    path: &str,
    response_headers: &HeaderMap,
    body: &'static [u8],
    received: u64,
    tags: &[&[u8]],
) -> oxiroute_cache::CacheKey {
    let entry = cache
        .prepare(
            request(&Method::GET, path, request_headers),
            StatusCode::OK,
            response_headers,
            Bytes::from_static(body),
            timing(received),
            tags,
        )
        .expect("prepared entry");
    let key = entry.key().clone();
    match cache.begin_fill(key.base().clone()).expect("fill") {
        FillJoin::Leader(leader) => {
            assert!(matches!(
                leader.store(entry).expect("store entry"),
                StoreOutcome::Stored { .. }
            ));
        }
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("unexpected fill join"),
    }
    key
}
