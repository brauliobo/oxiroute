#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) async fn finish_cache_revalidation(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    response: &pingora::http::ResponseHeader,
) -> pingora::Result<()> {
    let transaction = ctx
        .cache_transaction
        .as_mut()
        .expect("revalidation has a cache transaction");
    let timing = ResponseTiming {
        request_started: transaction.request().request_started,
        response_received: transaction.now(),
        response_received_wall: SystemTime::now(),
    };
    let cached = transaction
        .finish_revalidation(&response.headers, timing)
        .await;
    finish_cache_response(session, ctx, cached).await
}

pub(super) async fn finish_cache_stale_response(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    response: CachedResponse,
) -> pingora::Result<()> {
    if let Some(transaction) = ctx.cache_transaction.as_mut() {
        transaction.complete_without_store();
    }
    finish_cache_response(session, ctx, response).await
}

async fn finish_cache_response(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    response: CachedResponse,
) -> pingora::Result<()> {
    ctx.cache_capture = None;
    ctx.cache_response_handled = true;
    ctx.cache_transaction
        .as_ref()
        .expect("cached response has a transaction")
        .record_hit();
    write_cached_response_conditionally(session, &response).await?;
    Err(Error::new_in(ErrorType::InternalError))
}

pub(super) async fn cache_request_filter(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    cache: Arc<HttpCachePlan>,
    method: &Method,
    uri: &http::Uri,
) -> pingora::Result<bool> {
    let Some(authority) = ctx.authority.as_ref() else {
        return Ok(false);
    };
    let headers = session.req_header().headers.clone();
    if cache_request_bypasses_cache(&headers) {
        return Ok(false);
    }
    let scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let request = cache_request(cache.as_ref(), authority, method, uri, headers, scheme);
    match CacheTransaction::new(cache, request, ctx.listener.clone())
        .start()
        .await
    {
        CacheStart::Bypass(failure) => {
            if let CacheStartFailure::Lookup(error) = failure
                && !error.is_invalid_request()
            {
                warn!("cache lookup bypassed after validation failure: {error}");
            }
            Ok(false)
        }
        CacheStart::Hit(response) => {
            ctx.cache_response_handled = true;
            write_cached_response_conditionally(session, &response).await?;
            Ok(true)
        }
        CacheStart::OnlyIfCached => {
            respond_status(session, 504).await?;
            Ok(true)
        }
        CacheStart::MissLeader(transaction) | CacheStart::RevalidationLeader(transaction) => {
            ctx.cache_transaction = Some(transaction);
            Ok(false)
        }
    }
}

fn cache_request(
    cache: &HttpCachePlan,
    authority: &Authority,
    method: &Method,
    uri: &http::Uri,
    headers: HeaderMap,
    scheme: &'static str,
) -> CacheRequest {
    CacheRequest {
        method: method.clone(),
        scheme,
        authority: authority.as_str().to_owned(),
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        headers,
        request_started: cache.cache.now(),
    }
}

pub(super) async fn cache_purge_filter(
    session: &mut Session,
    _ctx: &mut HttpRequestContext,
    cache: Arc<HttpCachePlan>,
    method: &Method,
    uri: &http::Uri,
) -> pingora::Result<bool> {
    let Some(access) = cache.purge_access.as_ref() else {
        return Ok(false);
    };
    if !access.authorizes(&session.req_header().headers) {
        let head = session.req_header().method == Method::HEAD;
        write_local_response(
            session,
            401,
            &[(WWW_AUTHENTICATE, access.challenge().clone())],
            Bytes::new(),
            head,
        )
        .await?;
        return Ok(true);
    }
    let (authority_result, headers) = {
        let request = session.req_header();
        (request_authority(request), request.headers.clone())
    };
    let Ok(Some(authority)) = authority_result else {
        respond_status(session, 400).await?;
        return Ok(true);
    };
    let scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let request = cache_request(
        cache.as_ref(),
        &authority,
        method,
        uri,
        headers.clone(),
        scheme,
    );
    let tag = cache
        .surrogate_header
        .as_ref()
        .and_then(|header| headers.get(header));
    let result = if let Some(value) = tag {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b'"'))
        {
            write_local_response(
                session,
                400,
                &[],
                Bytes::from_static(b"invalid cache purge tag\n"),
                false,
            )
            .await?;
            return Ok(true);
        }
        cache.cache.purge_tag(bytes).await
    } else {
        match cache.cache.base(&request) {
            Ok(base) => cache.cache.purge_base(&base).await,
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(result) => {
            let status = if result.entries == 0 { 404 } else { 200 };
            let head = session.req_header().method == Method::HEAD;
            write_local_response(
                session,
                status,
                &[(
                    HeaderName::from_static("cache-control"),
                    HeaderValue::from_static("private, no-store"),
                )],
                Bytes::new(),
                head,
            )
            .await?;
        }
        Err(error) => {
            warn!("cache purge failed: {error}");
            let status = if error.is_invalid_request() { 400 } else { 503 };
            respond_status(session, status).await?;
        }
    }
    Ok(true)
}

pub(super) async fn finish_cache_fill(ctx: &mut HttpRequestContext) {
    let Some(capture) = ctx.cache_capture.take() else {
        if let Some(transaction) = ctx.cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        return;
    };
    let Some(transaction) = ctx.cache_transaction.as_mut() else {
        return;
    };
    let CacheCapture {
        status,
        headers,
        body,
        tags,
        timing,
        complete,
        admissible,
    } = capture;
    let tags_valid = transaction.cache_tags_within_limits(&tags);
    if !complete
        || !admissible
        || !tags_valid
        || !response_representation_valid(status, &headers, body.len())
    {
        transaction.complete_without_store();
        return;
    }
    let tag_refs = tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
    let prepared =
        transaction.prepare_response(status, &headers, Bytes::from(body), timing, &tag_refs);
    let Ok(entry) = prepared else {
        transaction.complete_without_store();
        return;
    };
    let _ = transaction.admit(entry).await;
}

fn response_representation_valid(status: StatusCode, headers: &HeaderMap, body_len: usize) -> bool {
    if status.is_informational()
        || status == StatusCode::SWITCHING_PROTOCOLS
        || status == StatusCode::NOT_MODIFIED
    {
        return false;
    }
    if matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT) && body_len != 0 {
        return false;
    }
    let mut lengths = headers.get_all(CONTENT_LENGTH).iter();
    let Some(length) = lengths.next() else {
        return true;
    };
    lengths.next().is_none()
        && length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            == Some(body_len)
}

pub(super) fn response_surrogate_tags(headers: &HeaderMap, name: &HeaderName) -> Vec<Bytes> {
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

pub(crate) fn cache_request_bypasses_cache(headers: &HeaderMap) -> bool {
    headers.contains_key(RANGE)
        || headers.contains_key(IF_RANGE)
        || headers.contains_key(IF_MATCH)
        || headers.contains_key(IF_UNMODIFIED_SINCE)
}

async fn write_cached_response_conditionally(
    session: &mut Session,
    response: &CachedResponse,
) -> pingora::Result<()> {
    if cached_response_matches_condition(session, response) {
        let mut not_modified = response.clone();
        not_modified.status = StatusCode::NOT_MODIFIED;
        not_modified.body = Bytes::new();
        write_cached_response(session, &not_modified).await
    } else {
        write_cached_response(session, response).await
    }
}

fn cached_response_matches_condition(session: &Session, response: &CachedResponse) -> bool {
    let request = session.req_header();
    if let Some(if_none_match) = request.headers.get(IF_NONE_MATCH) {
        return cached_etag_list_matches(
            if_none_match.as_bytes(),
            response.headers.get(ETAG).map(HeaderValue::as_bytes),
        );
    }
    if !matches!(request.method, Method::GET | Method::HEAD) {
        return false;
    }
    let Some(if_modified_since) = request.headers.get(IF_MODIFIED_SINCE) else {
        return false;
    };
    let Some(last_modified) = response.headers.get(LAST_MODIFIED) else {
        return false;
    };
    let Ok(if_modified_since_text) = if_modified_since.to_str() else {
        return false;
    };
    let Ok(if_modified_since) = httpdate::parse_http_date(if_modified_since_text) else {
        return false;
    };
    let Ok(last_modified_text) = last_modified.to_str() else {
        return false;
    };
    let Ok(last_modified) = httpdate::parse_http_date(last_modified_text) else {
        return false;
    };
    last_modified <= if_modified_since
}

pub(super) fn cached_etag_list_matches(value: &[u8], current: Option<&[u8]>) -> bool {
    value
        .split(|byte| *byte == b',')
        .map(trim_ows)
        .any(|candidate| {
            candidate == b"*" || current.is_some_and(|current| weak_etag_equal(candidate, current))
        })
}

fn weak_etag_equal(left: &[u8], right: &[u8]) -> bool {
    strip_weak_etag(left) == strip_weak_etag(right)
}

fn strip_weak_etag(value: &[u8]) -> &[u8] {
    let value = trim_ows(value);
    value
        .strip_prefix(b"W/")
        .or_else(|| value.strip_prefix(b"w/"))
        .unwrap_or(value)
}

fn trim_ows(value: &[u8]) -> &[u8] {
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

pub(super) async fn write_cached_response(
    session: &mut Session,
    response: &CachedResponse,
) -> pingora::Result<()> {
    let body_forbidden = matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let mut header = pingora::http::ResponseHeader::build(
        response.status,
        Some(response.headers.len().saturating_add(1)),
    )?;
    for (name, value) in &response.headers {
        header.append_header(name.clone(), value.clone())?;
    }
    if body_forbidden {
        header.remove_header(&CONTENT_LENGTH);
    } else {
        header.insert_header(CONTENT_LENGTH, response.body.len().to_string())?;
    }
    let head = session.req_header().method == Method::HEAD;
    let end = head || body_forbidden || response.body.is_empty();
    write_response_header_with_id(session, Box::new(header), end).await?;
    if !end {
        session
            .write_response_body(Some(response.body.clone()), true)
            .await?;
    }
    Ok(())
}
