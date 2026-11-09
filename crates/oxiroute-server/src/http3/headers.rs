#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
pub(super) const PROXY_CONNECTION: HeaderName = HeaderName::from_static("proxy-connection");

pub(super) fn request_deadline(
    service: &HttpServicePlan,
    downstream_timeouts: DownstreamTimeoutPolicy,
    started_at: Instant,
) -> Instant {
    let mut timeout = service.upstream_io_timeout();
    for value in [
        downstream_timeouts.client_timeout_ms,
        downstream_timeouts.request_timeout_ms,
    ]
    .into_iter()
    .flatten()
    {
        timeout = timeout.min(Duration::from_millis(value));
    }
    started_at + timeout
}

pub(super) fn request_authority(request: &Request<()>) -> Option<Authority> {
    request.uri().authority().cloned().or_else(|| {
        request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    })
}

pub(super) fn header_bytes(headers: &HeaderMap) -> usize {
    headers.iter().fold(0, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
    })
}

pub(crate) fn request_content_length(headers: &HeaderMap) -> Result<Option<u64>, ()> {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let length = values
        .next()
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or(())
        })
        .transpose()?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(length)
}

pub(super) fn h3_static_validator_headers(
    files: &crate::http::action::StaticFilesPlan,
    file: &StaticFile,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut headers = Vec::with_capacity(usize::from(files.etag_enabled()) + 1);
    if files.etag_enabled() {
        headers.push((HeaderName::from_static("etag"), file.etag.clone()));
    }
    headers.push((
        HeaderName::from_static("last-modified"),
        file.last_modified.clone(),
    ));
    headers
}

fn strip_h3_hop_by_hop_headers(headers: &mut HeaderMap) -> Result<(), ()> {
    let mut nominated = Vec::new();
    for value in headers.get_all(CONNECTION) {
        for token in value.to_str().map_err(|_| ())?.split(',') {
            nominated.push(HeaderName::from_bytes(token.trim().as_bytes()).map_err(|_| ())?);
        }
    }
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        CONNECTION,
        KEEP_ALIVE,
        HeaderName::from_static("proxy-authenticate"),
        HeaderName::from_static("proxy-authorization"),
        PROXY_CONNECTION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
    Ok(())
}

pub(crate) fn sanitize_h3_response_headers(
    headers: &mut HeaderMap,
    status: StatusCode,
    body_length: u64,
    head: bool,
) -> Result<(), ()> {
    if status.is_informational() || status == StatusCode::SWITCHING_PROTOCOLS {
        return Err(());
    }
    let declared_length = request_content_length(headers)?;
    strip_h3_hop_by_hop_headers(headers)?;
    let body_forbidden = matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let representation_length = if head {
        declared_length.unwrap_or(body_length)
    } else {
        body_length
    };
    if representation_length > H3_MAX_RESPONSE_BODY_BYTES {
        return Err(());
    }
    if body_forbidden {
        headers.remove(CONTENT_LENGTH);
    } else {
        headers.remove(CONTENT_LENGTH);
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&representation_length.to_string()).map_err(|_| ())?,
        );
    }
    if !head && body_forbidden && body_length != 0 {
        return Err(());
    }
    (header_bytes(headers) <= usize::try_from(H3_MAX_FIELD_SECTION_BYTES).map_err(|_| ())?)
        .then_some(())
        .ok_or(())
}

pub(crate) fn sanitize_h3_trailers(headers: &mut HeaderMap) -> Result<(), ()> {
    strip_h3_hop_by_hop_headers(headers)?;
    headers.remove(CONTENT_LENGTH);
    (header_bytes(headers) <= usize::try_from(H3_MAX_FIELD_SECTION_BYTES).map_err(|_| ())?)
        .then_some(())
        .ok_or(())
}

pub(super) async fn send_h3_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    status: StatusCode,
    headers: &[(HeaderName, HeaderValue)],
    body: Bytes,
    head: bool,
    deadline: Instant,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let body_forbidden = matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let body_length = u64::try_from(body.len()).map_err(|_| ()).ok();
    let Some(body_length) = body_length else {
        return false;
    };
    let Ok(mut response) = Response::builder().status(status).body(()) else {
        return false;
    };
    for (name, value) in headers {
        response.headers_mut().append(name.clone(), value.clone());
    }
    if sanitize_h3_response_headers(response.headers_mut(), status, body_length, head).is_err() {
        return false;
    }
    if !matches!(
        timeout_at(deadline, stream.send_response(response)).await,
        Ok(Ok(()))
    ) {
        return false;
    }
    if !head
        && !body_forbidden
        && !body.is_empty()
        && !matches!(
            timeout_at(deadline, stream.send_data(body)).await,
            Ok(Ok(()))
        )
    {
        return false;
    }
    matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(())))
}

pub(super) fn h3_redirect_location(
    location: &RedirectLocationPlan,
    authority: &Authority,
    uri: &Uri,
) -> Option<HeaderValue> {
    let host = normalized_redirect_host(authority);
    let request_uri = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    expand_redirect_location(
        location,
        RedirectContext {
            scheme: "https",
            normalized_host: Some(&host),
            request_uri,
        },
    )
}

pub(super) fn unix_time_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}
