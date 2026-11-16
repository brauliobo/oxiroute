use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{
    BaseKey, CacheControl, FillJoin, Lookup, RequestKeyInput, StoreOutcome, Vary,
};

use crate::common::{cache, request, response, store, timing};

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
