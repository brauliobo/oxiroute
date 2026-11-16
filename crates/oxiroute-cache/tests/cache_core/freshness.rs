use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    Cache, CacheControl, CacheError, Lookup, LookupStatus, MonoTime, ParseError, ResponseRejection,
    ResponseTiming, current_age,
};

use crate::common::{ManualClock, cache, config, request, response, store, timing};

#[test]
fn cache_control_is_strict_and_supports_extension_windows() {
    let mut headers = HeaderMap::new();
    headers.append(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, s-maxage=60, stale-while-revalidate=30"),
    );
    headers.append(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("stale-if-error=120, no-cache=\"set-cookie\""),
    );
    let parsed = CacheControl::parse(&headers).expect("strict directives");
    assert!(parsed.public && parsed.no_cache);
    assert_eq!(parsed.shared_max_age, Some(Duration::from_mins(1)));
    assert_eq!(parsed.stale_while_revalidate, Some(Duration::from_secs(30)));
    assert_eq!(parsed.stale_if_error, Some(Duration::from_mins(2)));

    headers.append(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("s-maxage=61"),
    );
    assert_eq!(
        CacheControl::parse(&headers),
        Err(ParseError::DuplicateDirective)
    );
    for invalid in [
        "max-age=-1",
        "max-age=\"5\"",
        "no-store=yes",
        "public,",
        "extension=\"a\"\"b\"",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_str(invalid).expect("wire header"),
        );
        assert!(CacheControl::parse(&headers).is_err(), "accepted {invalid}");
    }
}

#[test]
fn rfc_current_age_uses_apparent_age_delay_and_resident_time() {
    let timing = ResponseTiming {
        request_started: MonoTime::from_duration(Duration::ZERO),
        response_received: MonoTime::from_duration(Duration::from_secs(2)),
        response_received_wall: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
    };
    assert_eq!(
        current_age(
            Some(SystemTime::UNIX_EPOCH),
            Duration::from_secs(5),
            timing,
            MonoTime::from_duration(Duration::from_secs(22)),
        ),
        Duration::from_secs(30)
    );
}

#[test]
fn freshness_stale_windows_and_head_get_semantics_are_enforced() {
    let (cache, clock) = cache();
    let request_headers = HeaderMap::new();
    let key = store(
        &cache,
        &request_headers,
        "/asset",
        &response("max-age=10, stale-while-revalidate=5, stale-if-error=20"),
        b"body",
        0,
        &[],
    );

    clock.set(5);
    assert!(matches!(
        cache.lookup(request(&Method::HEAD, "/asset", &request_headers)),
        Ok(Lookup::Hit {
            status: LookupStatus::Hit,
            response
        }) if response.body == Bytes::from_static(b"body") && response.age == Duration::from_secs(5)
    ));
    clock.set(12);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/asset", &request_headers)),
        Ok(Lookup::Hit {
            status: LookupStatus::StaleWhileRevalidate,
            ..
        })
    ));
    clock.set(17);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/asset", &request_headers)),
        Ok(Lookup::Revalidate {
            stale_if_error: true,
            ..
        })
    ));
    assert_eq!(
        cache.stale_if_error(&key).expect("stale fallback").age,
        Duration::from_secs(17)
    );
    clock.set(31);
    assert!(cache.stale_if_error(&key).is_none());

    assert!(matches!(
        cache.prepare(
            request(&Method::HEAD, "/head", &request_headers),
            StatusCode::OK,
            &response("max-age=10"),
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::HeadOrUnsupportedMethod)
    ));
}

#[test]
fn stale_if_error_reuse_refreshes_memory_lru_order() {
    let mut limits = config();
    limits.max_entries = 2;
    let clock = Arc::new(ManualClock::default());
    let cache = Cache::with_clock(limits, clock.clone()).expect("cache");
    let request_headers = HeaderMap::new();
    let headers = response("max-age=1, stale-if-error=60");
    let stale_key = store(
        &cache,
        &request_headers,
        "/stale",
        &headers,
        b"stale",
        0,
        &[],
    );
    store(
        &cache,
        &request_headers,
        "/other",
        &headers,
        b"other",
        0,
        &[],
    );

    clock.set(2);
    assert_eq!(
        cache
            .stale_if_error(&stale_key)
            .expect("stale fallback")
            .body,
        Bytes::from_static(b"stale")
    );
    let new_headers = response("max-age=60");
    store(
        &cache,
        &request_headers,
        "/new",
        &new_headers,
        b"new",
        2,
        &[],
    );

    assert!(matches!(
        cache.lookup(request(&Method::GET, "/stale", &request_headers)),
        Ok(Lookup::Revalidate { .. })
    ));
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/other", &request_headers)),
        Ok(Lookup::Miss { .. })
    ));
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/new", &request_headers)),
        Ok(Lookup::Hit { .. })
    ));
}

#[test]
fn no_cache_always_validates_and_must_revalidate_disables_stale_extensions() {
    let (cache, clock) = cache();
    let request_headers = HeaderMap::new();
    store(
        &cache,
        &request_headers,
        "/no-cache",
        &response("no-cache, max-age=60"),
        b"x",
        0,
        &[],
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/no-cache", &request_headers)),
        Ok(Lookup::Revalidate { .. })
    ));
    store(
        &cache,
        &request_headers,
        "/must",
        &response("max-age=1, must-revalidate, stale-if-error=100"),
        b"x",
        0,
        &[],
    );
    clock.set(2);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/must", &request_headers)),
        Ok(Lookup::Revalidate {
            stale_if_error: false,
            ..
        })
    ));
}

#[test]
fn request_directives_bypass_or_constrain_reuse() {
    let (cache, _) = cache();
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/", &headers)),
        Ok(Lookup::Bypass { .. })
    ));
    headers.insert(http::header::RANGE, HeaderValue::from_static("bytes=0-1"));
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/", &headers)),
        Ok(Lookup::Bypass { .. })
    ));
    let empty = HeaderMap::new();
    assert!(matches!(
        cache.lookup(request(&Method::POST, "/", &empty)),
        Ok(Lookup::Bypass { .. })
    ));
    let mut conditional = HeaderMap::new();
    conditional.insert(
        http::header::IF_NONE_MATCH,
        HeaderValue::from_static("\"client\""),
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/", &conditional)),
        Ok(Lookup::Bypass { .. })
    ));
    let mut body = HeaderMap::new();
    body.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("1"));
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/", &body)),
        Ok(Lookup::Bypass { .. })
    ));
}

#[test]
fn expires_without_cache_control_sets_an_explicit_freshness_lifetime() {
    let (cache, clock) = cache();
    let request_headers = HeaderMap::new();
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::DATE,
        HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
    );
    headers.insert(
        http::header::EXPIRES,
        HeaderValue::from_static("Sun, 06 Nov 1994 08:50:37 GMT"),
    );
    store(
        &cache,
        &request_headers,
        "/expires",
        &headers,
        b"expires",
        0,
        &[],
    );
    clock.set(59);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/expires", &request_headers)),
        Ok(Lookup::Hit { .. })
    ));
    clock.set(61);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/expires", &request_headers)),
        Ok(Lookup::Revalidate { .. })
    ));
}

#[test]
fn invalid_dates_age_and_timing_are_rejected_without_panics() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    for (name, value) in [
        (http::header::DATE, "not-a-date"),
        (http::header::EXPIRES, "tomorrow"),
        (http::header::AGE, "-1"),
        (http::header::ETAG, "not-quoted"),
    ] {
        let mut headers = response("max-age=60");
        headers.insert(name, HeaderValue::from_str(value).expect("wire metadata"));
        assert!(matches!(
            cache.prepare(
                request(&Method::GET, "/", &request_headers),
                StatusCode::OK,
                &headers,
                Bytes::new(),
                timing(0),
                &[]
            ),
            Err(CacheError::ResponseRejected(
                ResponseRejection::InvalidMetadata(_)
            ))
        ));
    }
    let backwards = ResponseTiming {
        request_started: MonoTime::from_duration(Duration::from_secs(2)),
        response_received: MonoTime::from_duration(Duration::from_secs(1)),
        response_received_wall: SystemTime::UNIX_EPOCH,
    };
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::new(),
            backwards,
            &[]
        ),
        Err(CacheError::ResponseRejected(
            ResponseRejection::InvalidTiming
        ))
    ));
}
