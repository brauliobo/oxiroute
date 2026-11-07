#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) enum H3CacheDecision {
    Bypass,
    Respond(Box<CachedResponse>),
    OnlyIfCached,
    Continue(Box<CacheTransaction>),
}

pub(super) async fn start_h3_cache(
    cache: &Arc<HttpCachePlan>,
    request: &Request<()>,
    authority: &Authority,
    metrics: &ListenerMetrics,
) -> H3CacheDecision {
    let request = CacheRequest {
        method: request.method().clone(),
        scheme: "https",
        authority: authority.as_str().to_owned(),
        path: request.uri().path().to_owned(),
        query: request.uri().query().map(str::to_owned),
        headers: request.headers().clone(),
        request_started: cache.cache.now(),
    };
    match CacheTransaction::new(Arc::clone(cache), request, metrics.clone())
        .start()
        .await
    {
        CacheStart::Bypass(failure) => {
            if let CacheStartFailure::Lookup(error) = failure
                && !error.is_invalid_request()
            {
                warn!("HTTP/3 cache lookup bypassed after validation failure: {error}");
            }
            H3CacheDecision::Bypass
        }
        CacheStart::Hit(response) => H3CacheDecision::Respond(Box::new(response)),
        CacheStart::OnlyIfCached => H3CacheDecision::OnlyIfCached,
        CacheStart::MissLeader(transaction) | CacheStart::RevalidationLeader(transaction) => {
            H3CacheDecision::Continue(Box::new(transaction))
        }
    }
}

pub(super) async fn stale_h3_response(
    cache_transaction: &mut Option<CacheTransaction>,
) -> Option<CachedResponse> {
    let transaction = cache_transaction.as_mut()?;
    let response = transaction
        .stale_response(CacheFailureClass::Upstream)
        .await?;
    transaction.complete_without_store();
    transaction.record_hit();
    Some(response)
}

pub(super) async fn purge_h3_cache<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    authority: &Authority,
    cache: Arc<HttpCachePlan>,
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let access = cache
        .purge_access
        .as_ref()
        .expect("purge dispatch requires access policy");
    if !access.authorizes(request.headers()) {
        let headers = [(http::header::WWW_AUTHENTICATE, access.challenge().clone())];
        return send_h3_response(
            stream,
            StatusCode::UNAUTHORIZED,
            &headers,
            Bytes::new(),
            false,
            deadline,
        )
        .await
        .then_some(StatusCode::UNAUTHORIZED.as_u16());
    }
    let cache_request = CacheRequest {
        method: request.method().clone(),
        scheme: "https",
        authority: authority.as_str().to_owned(),
        path: request.uri().path().to_owned(),
        query: request.uri().query().map(str::to_owned),
        headers: request.headers().clone(),
        request_started: cache.cache.now(),
    };
    let result = if let Some(value) = cache
        .surrogate_header
        .as_ref()
        .and_then(|header| request.headers().get(header))
    {
        cache.cache.purge_tag(value.as_bytes()).await
    } else {
        match cache.cache.base(&cache_request) {
            Ok(base) => cache.cache.purge_base(&base).await,
            Err(error) => Err(error),
        }
    };
    let status = match result {
        Ok(result) if result.entries == 0 => StatusCode::NOT_FOUND,
        Ok(_) => StatusCode::OK,
        Err(error) => {
            warn!("HTTP/3 cache purge failed: {error}");
            if error.is_invalid_request() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    };
    let headers = [(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    )];
    send_h3_response(stream, status, &headers, Bytes::new(), false, deadline)
        .await
        .then_some(status.as_u16())
}

pub(super) async fn send_h3_cached_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    mut response: CachedResponse,
    deadline: Instant,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    if conditional_h3_cache_status(request, &response) == StatusCode::NOT_MODIFIED {
        response.status = StatusCode::NOT_MODIFIED;
        response.body = Bytes::new();
    }
    let headers = response
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    send_h3_response(
        stream,
        response.status,
        &headers,
        response.body,
        request.method() == Method::HEAD,
        deadline,
    )
    .await
}

pub(super) fn conditional_h3_cache_status(
    request: &Request<()>,
    response: &CachedResponse,
) -> StatusCode {
    if h3_cached_response_matches_condition(request, response) {
        StatusCode::NOT_MODIFIED
    } else {
        response.status
    }
}

fn h3_cached_response_matches_condition(request: &Request<()>, response: &CachedResponse) -> bool {
    if let Some(if_none_match) = request.headers().get(http::header::IF_NONE_MATCH) {
        return if_none_match
            .as_bytes()
            .split(|byte| *byte == b',')
            .map(trim_h3_ows)
            .any(|candidate| {
                candidate == b"*"
                    || response
                        .headers
                        .get(http::header::ETAG)
                        .is_some_and(|current| {
                            strip_h3_weak_etag(candidate) == strip_h3_weak_etag(current.as_bytes())
                        })
            });
    }
    if !matches!(request.method(), &Method::GET | &Method::HEAD) {
        return false;
    }
    let Some(if_modified_since) = request.headers().get(http::header::IF_MODIFIED_SINCE) else {
        return false;
    };
    let Some(last_modified) = response.headers.get(http::header::LAST_MODIFIED) else {
        return false;
    };
    let (Ok(if_modified_since), Ok(last_modified)) =
        (if_modified_since.to_str(), last_modified.to_str())
    else {
        return false;
    };
    let (Ok(if_modified_since), Ok(last_modified)) = (
        httpdate::parse_http_date(if_modified_since),
        httpdate::parse_http_date(last_modified),
    ) else {
        return false;
    };
    last_modified <= if_modified_since
}

fn trim_h3_ows(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(start, |index| index + 1);
    &value[start..end]
}

fn strip_h3_weak_etag(value: &[u8]) -> &[u8] {
    let value = trim_h3_ows(value);
    value
        .strip_prefix(b"W/")
        .or_else(|| value.strip_prefix(b"w/"))
        .unwrap_or(value)
}

pub(super) fn h3_cache_representation_valid(
    status: StatusCode,
    headers: &HeaderMap,
    body_len: usize,
) -> bool {
    if status.is_informational()
        || matches!(
            status,
            StatusCode::SWITCHING_PROTOCOLS | StatusCode::NOT_MODIFIED
        )
    {
        return false;
    }
    if matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT) && body_len != 0 {
        return false;
    }
    request_content_length(headers)
        .ok()
        .flatten()
        .is_none_or(|length| length == u64::try_from(body_len).unwrap_or(u64::MAX))
}

pub(super) fn h3_surrogate_tags(headers: &HeaderMap, name: &HeaderName) -> Vec<Bytes> {
    headers
        .get_all(name)
        .iter()
        .flat_map(|value| {
            value
                .as_bytes()
                .split(u8::is_ascii_whitespace)
                .filter(|tag| !tag.is_empty())
                .map(Bytes::copy_from_slice)
        })
        .collect()
}
