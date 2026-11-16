use std::time::SystemTime;

use http::Method;
use oxiroute_cache::{CacheConfig, FillJoin, Lookup, RequestKeyInput, ResponseTiming};

use super::*;

fn page() -> Bytes {
    Bytes::from(
        (0..400)
            .flat_map(|row| format!("<li class=\"row\">item {row}</li>\n").into_bytes())
            .collect::<Vec<_>>(),
    )
}

fn response_headers(content_type: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(ETAG, HeaderValue::from_static("\"v1\""));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=600"),
    );
    headers
}

fn request(path: &str, headers: &HeaderMap) -> oxiroute_cache::RequestKeyInput<'static> {
    RequestKeyInput {
        method: &Method::GET,
        scheme: "https",
        authority: "example.com",
        path: Box::leak(path.to_owned().into_boxed_str()),
        query: None,
        headers: Box::leak(Box::new(headers.clone())),
    }
}

fn compression(precompress: &[CacheCompressionCoding]) -> CacheCompression {
    let policy = HttpCacheCompression {
        precompress: precompress.to_vec(),
        ..HttpCacheCompression::default()
    };
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    CacheCompression::new(CompressionPolicy::compile(&policy), Arc::new(cache))
}

fn prepare(compression: &CacheCompression, content_type: &'static str) -> PreparedEntry {
    let now = compression.cache.now();
    compression
        .cache
        .prepare(
            request("/page", &HeaderMap::new()),
            StatusCode::OK,
            &response_headers(content_type),
            page(),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )
        .expect("prepared entry")
}

fn store(compression: &CacheCompression, entry: PreparedEntry) -> CachedResponse {
    let cache = &compression.cache;
    let FillJoin::Leader(leader) = cache.begin_fill(entry.key().base().clone()).expect("fill")
    else {
        panic!("expected leader");
    };
    leader.store(entry).expect("stored");
    match cache
        .lookup(request("/page", &HeaderMap::new()))
        .expect("lookup")
    {
        Lookup::Hit { response, .. } => response,
        _ => panic!("expected a hit"),
    }
}

fn accepting(coding: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::ACCEPT_ENCODING,
        HeaderValue::from_static(coding),
    );
    headers
}

#[tokio::test]
async fn precompressed_codings_are_charged_when_the_response_is_stored() {
    let compression = compression(&[CacheCompressionCoding::Gzip, CacheCompressionCoding::Zstd]);
    let mut entry = prepare(&compression, "text/html");
    let plain = entry.charge();
    compression
        .precompress(
            &mut entry,
            StatusCode::OK,
            &response_headers("text/html"),
            &page(),
        )
        .await;
    let charged = entry.charge();
    assert!(charged > plain);
    let hit = store(&compression, entry);
    assert_eq!(compression.cache.stats().bytes_used, charged);

    let gzip = compression.encode(&accepting("gzip"), hit.clone()).await;
    assert_eq!(gzip.headers[CONTENT_ENCODING], "gzip");
    assert_eq!(gzip.headers[ETAG], "W/\"v1\"");
    assert_eq!(
        gzip.headers[CONTENT_LENGTH],
        gzip.body.len().to_string().as_str()
    );
    assert_eq!(compression.cache.stats().bytes_used, charged);

    let brotli = compression.encode(&accepting("br"), hit).await;
    assert_eq!(brotli.headers[CONTENT_ENCODING], "br");
    assert!(compression.cache.stats().bytes_used > charged);
}

#[tokio::test]
async fn nothing_is_precompressed_for_other_content_types() {
    let compression = compression(&[CacheCompressionCoding::Gzip]);
    let mut entry = prepare(&compression, "image/png");
    let plain = entry.charge();
    compression
        .precompress(
            &mut entry,
            StatusCode::OK,
            &response_headers("image/png"),
            &page(),
        )
        .await;
    assert_eq!(entry.charge(), plain);
}

#[tokio::test]
async fn responses_without_a_variant_keep_their_body_and_headers() {
    let compression = compression(&[]);
    let hit = store(&compression, prepare(&compression, "text/html"));
    let identity = compression
        .encode(&accepting("identity"), hit.clone())
        .await;
    assert!(identity.headers.get(CONTENT_ENCODING).is_none());
    assert_eq!(identity.headers[ETAG], "\"v1\"");
    assert_eq!(identity.body, hit.body);
    assert!(
        identity
            .headers
            .get(VARY)
            .is_some_and(|vary| vary == "Accept-Encoding")
    );
}
