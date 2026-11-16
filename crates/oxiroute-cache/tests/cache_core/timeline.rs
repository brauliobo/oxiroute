use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    CacheResponse, CacheTimeline, CacheTimelineError, FillJoin, Lookup, LookupStatus,
};

use crate::common::{cache, request, response, timing};

#[test]
fn canonical_timeline_uses_status_ttl_failure_grace_and_revalidation_keep() {
    let (cache, clock) = cache();
    let request_headers = HeaderMap::new();
    let timeline = CacheTimeline::new(
        true,
        Duration::from_secs(30),
        [(StatusCode::OK, Duration::from_secs(5))],
        Duration::from_secs(3),
        Duration::from_secs(10),
    )
    .expect("canonical timeline");
    let entry = cache
        .prepare_with_timeline(
            request(&Method::GET, "/timeline", &request_headers),
            CacheResponse {
                status: StatusCode::OK,
                headers: &response("max-age=100, stale-while-revalidate=100, stale-if-error=100"),
                body: Bytes::from_static(b"timeline"),
                timing: timing(0),
                tags: &[],
            },
            &timeline,
        )
        .expect("timeline entry");
    let base = entry.key().base().clone();
    match cache.begin_fill(base).expect("fill") {
        FillJoin::Leader(leader) => {
            leader.store(entry).expect("store timeline entry");
        }
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    }

    clock.set(4);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/timeline", &request_headers)),
        Ok(Lookup::Hit {
            status: LookupStatus::Hit,
            ..
        })
    ));

    clock.set(6);
    let mut max_stale = HeaderMap::new();
    max_stale.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("max-stale=100"),
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/timeline", &max_stale)),
        Ok(Lookup::Revalidate {
            stale_if_error: true,
            ..
        })
    ));

    clock.set(9);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/timeline", &request_headers)),
        Ok(Lookup::Revalidate {
            stale_if_error: false,
            ..
        })
    ));

    clock.set(16);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/timeline", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert_eq!(cache.stats().entries, 0);
}

#[test]
fn canonical_timeline_origin_and_default_ttl_precedence_is_explicit() {
    let origin = CacheTimeline::new(
        true,
        Duration::from_secs(30),
        [],
        Duration::ZERO,
        Duration::from_mins(1),
    )
    .expect("origin timeline");
    let configured = CacheTimeline::new(
        false,
        Duration::from_secs(30),
        [],
        Duration::ZERO,
        Duration::from_mins(1),
    )
    .expect("configured timeline");
    let request_headers = HeaderMap::new();

    for (path, timeline) in [("/origin", &origin), ("/configured", &configured)] {
        let (cache, clock) = cache();
        let entry = cache
            .prepare_with_timeline(
                request(&Method::GET, path, &request_headers),
                CacheResponse {
                    status: StatusCode::OK,
                    headers: &response("max-age=2"),
                    body: Bytes::new(),
                    timing: timing(0),
                    tags: &[],
                },
                timeline,
            )
            .expect("timeline entry");
        match cache.begin_fill(entry.key().base().clone()).expect("fill") {
            FillJoin::Leader(leader) => {
                leader.store(entry).expect("store entry");
            }
            FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
        }
        clock.set(3);
        let lookup = cache
            .lookup(request(&Method::GET, path, &request_headers))
            .expect("lookup");
        if path == "/origin" {
            assert!(matches!(lookup, Lookup::Revalidate { .. }));
        } else {
            assert!(matches!(lookup, Lookup::Hit { .. }));
        }
    }
}

#[test]
fn canonical_timeline_rejects_ambiguous_or_unstorable_windows() {
    assert_eq!(
        CacheTimeline::new(
            true,
            Duration::ZERO,
            [],
            Duration::from_secs(2),
            Duration::from_secs(1),
        ),
        Err(CacheTimelineError::GraceExceedsKeep)
    );
    assert_eq!(
        CacheTimeline::new(
            true,
            Duration::ZERO,
            [
                (StatusCode::OK, Duration::ZERO),
                (StatusCode::OK, Duration::from_secs(1)),
            ],
            Duration::ZERO,
            Duration::ZERO,
        ),
        Err(CacheTimelineError::DuplicateStatus)
    );
    for status in [
        StatusCode::EARLY_HINTS,
        StatusCode::PARTIAL_CONTENT,
        StatusCode::NOT_MODIFIED,
    ] {
        assert_eq!(
            CacheTimeline::new(
                true,
                Duration::ZERO,
                [(status, Duration::ZERO)],
                Duration::ZERO,
                Duration::ZERO,
            ),
            Err(CacheTimelineError::UnsupportedStatus)
        );
    }
}
