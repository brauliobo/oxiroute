use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    BaseKey, Cache, CacheConfig, CacheControl, CacheError, CacheResponse, CacheTimeline,
    CacheTimelineError, Clock, FillJoin, FillOutcome, Lookup, LookupStatus, MonoTime, ParseError,
    RequestKeyInput, ResponseRejection, ResponseTiming, StoreOutcome, Vary, current_age,
    merge_not_modified_headers,
};

#[derive(Default)]
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, seconds: u64) {
        self.0.store(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> MonoTime {
        MonoTime::from_duration(Duration::from_secs(self.0.load(Ordering::SeqCst)))
    }
}

fn config() -> CacheConfig {
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

fn cache() -> (Cache, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::default());
    let cache = Cache::with_clock(config(), clock.clone()).expect("valid cache");
    (cache, clock)
}

fn request<'a>(method: &'a Method, path: &'a str, headers: &'a HeaderMap) -> RequestKeyInput<'a> {
    RequestKeyInput {
        method,
        scheme: "HTTPS",
        authority: "EXAMPLE.COM:443",
        path,
        query: Some("a=1&b=2"),
        headers,
    }
}

fn timing(received: u64) -> ResponseTiming {
    ResponseTiming {
        request_started: MonoTime::from_duration(Duration::from_secs(received.saturating_sub(2))),
        response_received: MonoTime::from_duration(Duration::from_secs(received)),
        response_received_wall: SystemTime::UNIX_EPOCH
            + Duration::from_secs(784_111_777 + received),
    }
}

fn response(cache_control: &str) -> HeaderMap {
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

fn store(
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

#[test]
fn canonical_keys_share_head_and_get_and_normalize_vary_values() {
    let mut first_headers = HeaderMap::new();
    first_headers.append(
        http::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static(" en-US\t "),
    );
    first_headers.append(
        http::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("fr"),
    );
    let mut second_headers = HeaderMap::new();
    second_headers.append(
        http::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US"),
    );
    second_headers.append(
        http::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static(" fr "),
    );
    let vary_headers = {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::VARY,
            HeaderValue::from_static("User-Agent, ACCEPT-LANGUAGE, user-agent"),
        );
        headers
    };
    let vary = Vary::parse(&vary_headers, 8, 1024).expect("vary");
    assert_eq!(vary.names().expect("concrete vary").len(), 2);

    let get =
        BaseKey::new(request(&Method::GET, "/asset", &first_headers), 1024).expect("GET base");
    let head =
        BaseKey::new(request(&Method::HEAD, "/asset", &second_headers), 1024).expect("HEAD base");
    assert_eq!(get, head);
    let first = oxiroute_cache::CacheKey::new(get, &first_headers, &vary, 1024).expect("first key");
    let second =
        oxiroute_cache::CacheKey::new(head, &second_headers, &vary, 1024).expect("second key");
    assert_eq!(first, second);

    let without_default_port = RequestKeyInput {
        authority: "example.com",
        ..request(&Method::GET, "/asset", &first_headers)
    };
    assert_eq!(
        BaseKey::new(without_default_port, 1024).expect("canonical authority"),
        *first.base()
    );
}

#[test]
fn absent_and_present_empty_vary_fields_are_distinct() {
    let mut vary_headers = HeaderMap::new();
    vary_headers.insert(http::header::VARY, HeaderValue::from_static("x-empty"));
    let vary = Vary::parse(&vary_headers, 2, 64).expect("vary");
    let absent = HeaderMap::new();
    let mut empty = HeaderMap::new();
    empty.insert("x-empty", HeaderValue::from_static(""));
    let base = BaseKey::new(request(&Method::GET, "/", &absent), 1024).expect("base");
    assert_ne!(
        oxiroute_cache::CacheKey::new(base.clone(), &absent, &vary, 1024).expect("absent"),
        oxiroute_cache::CacheKey::new(base, &empty, &vary, 1024).expect("empty")
    );
}

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
fn unsafe_responses_fail_closed() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    for (control, expected) in [
        ("no-store", ResponseRejection::NoStore),
        ("private, max-age=60", ResponseRejection::Private),
    ] {
        let error = cache
            .prepare(
                request(&Method::GET, "/", &request_headers),
                StatusCode::OK,
                &response(control),
                Bytes::new(),
                timing(0),
                &[],
            )
            .expect_err("unsafe response");
        assert!(matches!(error, CacheError::ResponseRejected(reason) if reason == expected));
    }

    let mut set_cookie = response("public, max-age=60");
    set_cookie.insert(
        http::header::SET_COOKIE,
        HeaderValue::from_static("sid=secret"),
    );
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &set_cookie,
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::ResponseRejected(ResponseRejection::SetCookie))
    ));

    let mut vary_any = response("public, max-age=60");
    vary_any.insert(http::header::VARY, HeaderValue::from_static("*"));
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &vary_any,
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::ResponseRejected(ResponseRejection::VaryAny))
    ));
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::PARTIAL_CONTENT,
            &response("public, max-age=60"),
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::ResponseRejected(ResponseRejection::Status))
    ));
    let mut hop_by_hop = response("public, max-age=60");
    hop_by_hop.insert(http::header::CONNECTION, HeaderValue::from_static("close"));
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &hop_by_hop,
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::ResponseRejected(ResponseRejection::HopByHop))
    ));
}

#[test]
fn authenticated_responses_require_explicit_shared_permission() {
    let (cache, _) = cache();
    let mut request_headers = HeaderMap::new();
    request_headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer secret"),
    );
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::ResponseRejected(
            ResponseRejection::Authorization
        ))
    ));
    assert!(
        cache
            .prepare(
                request(&Method::GET, "/", &request_headers),
                StatusCode::OK,
                &response("public, max-age=60"),
                Bytes::new(),
                timing(0),
                &[]
            )
            .is_ok()
    );
}

#[test]
fn cached_unauthenticated_responses_are_not_reused_for_authorized_requests() {
    let (cache, _) = cache();
    let anonymous = HeaderMap::new();
    let mut authorized = HeaderMap::new();
    authorized.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer secret"),
    );
    store(
        &cache,
        &anonymous,
        "/ordinary",
        &response("max-age=60"),
        b"ordinary",
        0,
        &[],
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/ordinary", &authorized)),
        Ok(Lookup::Miss { .. })
    ));
    store(
        &cache,
        &anonymous,
        "/public",
        &response("public, max-age=60"),
        b"public",
        0,
        &[],
    );
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/public", &authorized)),
        Ok(Lookup::Hit { .. })
    ));
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
fn validators_apply_and_304_metadata_replaces_stored_fields() {
    let (cache, clock) = cache();
    let request_headers = HeaderMap::new();
    let mut initial = response("max-age=1");
    initial.insert(http::header::ETAG, HeaderValue::from_static("\"v1\""));
    initial.insert("x-origin", HeaderValue::from_static("old"));
    let key = store(
        &cache,
        &request_headers,
        "/validated",
        &initial,
        b"entity",
        0,
        &[],
    );
    clock.set(2);
    let validators = match cache
        .lookup(request(&Method::GET, "/validated", &request_headers))
        .expect("lookup")
    {
        Lookup::Revalidate { validators, .. } => validators,
        other => panic!("unexpected lookup: {other:?}"),
    };
    let mut conditional = HeaderMap::new();
    validators.apply(&mut conditional);
    assert_eq!(
        conditional.get(http::header::IF_NONE_MATCH),
        Some(&HeaderValue::from_static("\"v1\""))
    );

    let mut not_modified = HeaderMap::new();
    not_modified.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=60"),
    );
    not_modified.insert("x-origin", HeaderValue::from_static("new"));
    let replacement = cache
        .prepare_not_modified(
            request(&Method::GET, "/validated", &request_headers),
            &key,
            &not_modified,
            timing(2),
        )
        .expect("304 replacement");
    match cache.begin_fill(key.base().clone()).expect("fill") {
        FillJoin::Leader(leader) => {
            leader.store(replacement).expect("replace entry");
        }
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    }
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/validated", &request_headers)),
        Ok(Lookup::Hit { response, .. })
            if response.body == Bytes::from_static(b"entity")
                && response.headers.get("x-origin") == Some(&HeaderValue::from_static("new"))
    ));

    let mut prohibited = HeaderMap::new();
    prohibited.insert(http::header::SET_COOKIE, HeaderValue::from_static("x=y"));
    assert!(merge_not_modified_headers(&initial, &prohibited).is_err());
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

#[test]
fn configured_header_body_object_and_tag_bounds_are_hard() {
    let mut limits = config();
    limits.max_header_bytes = 8;
    limits.max_body_bytes = 3;
    limits.max_object_bytes = 512;
    let bounded_cache = Cache::with_clock(limits, Arc::new(ManualClock::default())).expect("cache");
    let request_headers = HeaderMap::new();
    assert!(matches!(
        bounded_cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::new(),
            timing(0),
            &[]
        ),
        Err(CacheError::HeadersTooLarge)
    ));

    let (cache, _) = cache();
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::from(vec![0; 4097]),
            timing(0),
            &[]
        ),
        Err(CacheError::BodyTooLarge)
    ));
    assert!(matches!(
        cache.prepare(
            request(&Method::GET, "/", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::new(),
            timing(0),
            &[b"invalid tag"]
        ),
        Err(CacheError::InvalidTag)
    ));
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
fn vary_variants_and_bounded_surrogate_purge_are_exact() {
    let (cache, _) = cache();
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
    let mut headers = response("max-age=60");
    headers.insert(
        http::header::VARY,
        HeaderValue::from_static("accept-language"),
    );
    let en_key = store(&cache, &en, "/vary", &headers, b"hello", 0, &[b"page"]);
    store(&cache, &fr, "/vary", &headers, b"bonjour", 0, &[b"page"]);
    assert!(matches!(
        cache.lookup(request(&Method::GET, "/vary", &fr)),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"bonjour")
    ));
    assert_eq!(cache.purge_exact(&en_key).entries, 1);
    assert_eq!(cache.purge_tag(b"page").expect("tag purge").entries, 1);
    assert_eq!(cache.stats().entries, 0);
    assert!(cache.purge_tag(&[b'x'; 33]).is_err());

    let candidate = cache
        .prepare(
            request(&Method::GET, "/in-flight-tag", &en),
            StatusCode::OK,
            &headers,
            Bytes::from_static(b"late"),
            timing(0),
            &[b"page"],
        )
        .expect("tagged candidate");
    let leader = match cache
        .begin_fill(candidate.key().base().clone())
        .expect("fill")
    {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    };
    assert_eq!(
        cache
            .purge_tag(b"page")
            .expect("in-flight purge")
            .fills_cancelled,
        0
    );
    assert_eq!(
        leader.store(candidate).expect("obsolete fill"),
        StoreOutcome::Stored { evicted: 0 }
    );
    assert_eq!(
        cache.purge_tag(b"page").expect("stored tag purge").entries,
        1
    );
}

#[test]
fn parser_property_sweep_never_panics_on_arbitrary_header_bytes() {
    let mut state = 0x9e37_79b9_u32;
    for length in 0..128 {
        for _ in 0..64 {
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..length {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                bytes.push((state >> 24) as u8);
            }
            if let Ok(value) = HeaderValue::from_bytes(&bytes) {
                let mut headers = HeaderMap::new();
                headers.insert(http::header::CACHE_CONTROL, value.clone());
                let _ = CacheControl::parse(&headers);
                headers.clear();
                headers.insert(http::header::VARY, value);
                let _ = Vary::parse(&headers, 8, 256);
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn collapsed_forwarding_wakes_all_followers_after_lock_free_fill() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    let entry = cache
        .prepare(
            request(&Method::GET, "/collapse", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::from_static(b"one"),
            timing(0),
            &[],
        )
        .expect("entry");
    let leader = match cache.begin_fill(entry.key().base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    };
    let mut followers = Vec::new();
    for _ in 0..32 {
        match cache.begin_fill(entry.key().base().clone()).expect("fill") {
            FillJoin::Follower(waiter) => followers.push(tokio::spawn(waiter.wait())),
            FillJoin::Leader(_) | FillJoin::AtCapacity => panic!("follower"),
        }
    }
    assert_eq!(cache.stats().in_flight, 1);
    leader.store(entry).expect("store");
    for follower in followers {
        assert_eq!(follower.await.expect("join"), FillOutcome::Stored);
    }
    assert_eq!(cache.stats().fill_followers, 32);
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

#[tokio::test(flavor = "current_thread")]
async fn purge_and_drop_are_generation_safe_and_wake_followers() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    let first = cache
        .prepare(
            request(&Method::GET, "/generation", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::from_static(b"old"),
            timing(0),
            &[],
        )
        .expect("old entry");
    let old_leader = match cache.begin_fill(first.key().base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("old leader"),
    };
    let purged_waiter = match cache.begin_fill(first.key().base().clone()).expect("fill") {
        FillJoin::Follower(waiter) => waiter,
        FillJoin::Leader(_) | FillJoin::AtCapacity => panic!("old follower"),
    };
    assert_eq!(cache.purge_exact(first.key()).fills_cancelled, 1);
    assert_eq!(purged_waiter.wait().await, FillOutcome::Purged);

    let new_leader = match cache.begin_fill(first.key().base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("new leader"),
    };
    assert_eq!(
        old_leader.store(first.clone()).expect("old completion"),
        StoreOutcome::GenerationLost
    );
    new_leader.store(first).expect("new completion");

    let cancelled_leader = match cache
        .begin_fill(
            BaseKey::new(request(&Method::GET, "/cancel", &request_headers), 1024).expect("base"),
        )
        .expect("fill")
    {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("cancel leader"),
    };
    let cancelled_waiter = match cache
        .begin_fill(
            BaseKey::new(request(&Method::GET, "/cancel", &request_headers), 1024).expect("base"),
        )
        .expect("fill")
    {
        FillJoin::Follower(waiter) => waiter,
        FillJoin::Leader(_) | FillJoin::AtCapacity => panic!("cancel follower"),
    };
    drop(cancelled_leader);
    assert_eq!(cancelled_waiter.wait().await, FillOutcome::Cancelled);
    assert_eq!(cache.stats().in_flight, 0);
}

#[test]
fn fill_keys_and_follower_fanout_are_bounded() {
    let mut limits = config();
    limits.max_followers_per_fill = 1;
    let cache = Cache::with_clock(limits, Arc::new(ManualClock::default())).expect("cache");
    let request_headers = HeaderMap::new();
    let base = BaseKey::new(request(&Method::GET, "/fill", &request_headers), 1024).expect("base");
    let leader = match cache.begin_fill(base.clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    };
    assert!(matches!(
        cache.begin_fill(base.clone()),
        Ok(FillJoin::Follower(_))
    ));
    assert!(matches!(cache.begin_fill(base), Ok(FillJoin::AtCapacity)));

    let long_path = format!("/{}", "x".repeat(2048));
    let oversized = BaseKey::new(request(&Method::GET, &long_path, &request_headers), 4096)
        .expect("externally oversized base");
    assert!(matches!(
        cache.begin_fill(oversized),
        Err(CacheError::InvalidFillKey)
    ));
    drop(leader);
}
