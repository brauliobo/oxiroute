use std::sync::Arc;

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, StatusCode,
    header::{CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, ETAG, VARY},
};
use oxiroute_cache::{Cache, CachedResponse, PreparedEntry};
use oxiroute_compress::{AcceptEncoding, Coding, Levels, compress};
use oxiroute_config::{CacheCompressionCoding, HttpCacheCompression};

use super::HttpCachePlan;

#[derive(Clone, Debug)]
pub(crate) struct CompressionPolicy {
    codings: Box<[Coding]>,
    precompress: Box<[Coding]>,
    levels: Levels,
    content_types: Box<[String]>,
    min_length_bytes: usize,
}

impl CompressionPolicy {
    pub(crate) fn compile(policy: &HttpCacheCompression) -> Self {
        let codings =
            |codings: &[CacheCompressionCoding]| codings.iter().copied().map(coding).collect();
        Self {
            codings: codings(&policy.codings),
            precompress: codings(&policy.precompress),
            levels: Levels {
                gzip: policy.gzip_level,
                brotli: policy.brotli_level,
                zstd: policy.zstd_level,
            },
            content_types: policy.content_types.clone().into_boxed_slice(),
            min_length_bytes: usize::try_from(policy.min_length_bytes)
                .expect("validated compression length fits usize"),
        }
    }
}

const fn coding(configured: CacheCompressionCoding) -> Coding {
    match configured {
        CacheCompressionCoding::Zstd => Coding::Zstd,
        CacheCompressionCoding::Brotli => Coding::Brotli,
        CacheCompressionCoding::Gzip => Coding::Gzip,
    }
}

pub(crate) struct CacheCompression {
    policy: CompressionPolicy,
    cache: Arc<Cache>,
}

impl CacheCompression {
    pub(crate) const fn new(policy: CompressionPolicy, cache: Arc<Cache>) -> Self {
        Self { policy, cache }
    }

    fn applies(&self, status: StatusCode, headers: &HeaderMap, body_bytes: usize) -> bool {
        status == StatusCode::OK
            && body_bytes >= self.policy.min_length_bytes
            && !headers.contains_key(CONTENT_ENCODING)
            && !forbids_transform(headers)
            && self.compressible_type(headers)
    }

    fn compressible_type(&self, headers: &HeaderMap) -> bool {
        headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .is_some_and(|media_type| {
                self.policy
                    .content_types
                    .iter()
                    .any(|configured| configured.eq_ignore_ascii_case(media_type))
            })
    }

    async fn build(&self, coding: Coding, body: Bytes) -> Option<Bytes> {
        let levels = self.policy.levels;
        let built = tokio::task::spawn_blocking(move || {
            let encoded = compress(coding, levels, &body)?;
            Ok::<_, oxiroute_compress::CompressError>(
                (encoded.len() < body.len()).then_some(encoded),
            )
        })
        .await;
        match built {
            Ok(Ok(encoded)) => encoded.map(Bytes::from),
            Ok(Err(error)) => {
                log::warn!("cache {} compression failed: {error}", coding.token());
                None
            }
            Err(error) => {
                log::warn!("cache {} compression task failed: {error}", coding.token());
                None
            }
        }
    }

    pub(crate) async fn encode(
        &self,
        request: &HeaderMap,
        mut response: CachedResponse,
    ) -> CachedResponse {
        if !self.applies(response.status, &response.headers, response.body.len()) {
            return response;
        }
        vary_on_accept_encoding(&mut response.headers);
        let Some(coding) = AcceptEncoding::from_headers(request).select(&self.policy.codings)
        else {
            return response;
        };
        let variant = self
            .cache
            .variant(&response, coding, |body| self.build(coding, body))
            .await;
        if let Some(body) = variant {
            apply_coding(&mut response, coding, body);
        }
        response
    }

    pub(crate) async fn precompress(
        &self,
        entry: &mut PreparedEntry,
        status: StatusCode,
        headers: &HeaderMap,
        body: &Bytes,
    ) {
        if !self.applies(status, headers, body.len()) {
            return;
        }
        for coding in &self.policy.precompress {
            if let Some(variant) = self.build(*coding, body.clone()).await {
                self.cache.attach_variant(entry, *coding, variant);
            }
        }
    }
}

impl HttpCachePlan {
    pub(crate) async fn encode(
        &self,
        request: &HeaderMap,
        response: CachedResponse,
    ) -> CachedResponse {
        match &self.compression {
            Some(compression) => compression.encode(request, response).await,
            None => response,
        }
    }

    pub(crate) async fn precompress(
        &self,
        entry: &mut PreparedEntry,
        status: StatusCode,
        headers: &HeaderMap,
        body: &Bytes,
    ) {
        if let Some(compression) = &self.compression {
            compression.precompress(entry, status, headers, body).await;
        }
    }
}

fn apply_coding(response: &mut CachedResponse, coding: Coding, body: Bytes) {
    let headers = &mut response.headers;
    headers.insert(CONTENT_ENCODING, HeaderValue::from_static(coding.token()));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
    if let Some(tag) = headers.get(ETAG).and_then(weak_etag) {
        headers.insert(ETAG, tag);
    }
    response.body = body;
}

fn weak_etag(tag: &HeaderValue) -> Option<HeaderValue> {
    let bytes = tag.as_bytes();
    if bytes.starts_with(b"W/") {
        return None;
    }
    HeaderValue::from_bytes(&[b"W/", bytes].concat()).ok()
}

fn forbids_transform(headers: &HeaderMap) -> bool {
    header_tokens(headers, &CACHE_CONTROL).any(|token| token.eq_ignore_ascii_case(b"no-transform"))
}

fn vary_on_accept_encoding(headers: &mut HeaderMap) {
    let varies = header_tokens(headers, &VARY)
        .any(|token| token == b"*" || token.eq_ignore_ascii_case(b"accept-encoding"));
    if !varies {
        headers.append(VARY, HeaderValue::from_static("Accept-Encoding"));
    }
}

fn header_tokens<'a>(
    headers: &'a HeaderMap,
    name: &'a http::HeaderName,
) -> impl Iterator<Item = &'a [u8]> {
    headers
        .get_all(name)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .map(<[u8]>::trim_ascii)
}

#[cfg(test)]
mod tests;
