use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    Cache, CacheError, FillJoin, Lookup, ResponseRejection, merge_not_modified_headers,
};

use crate::common::{ManualClock, cache, config, request, response, store, timing};

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
