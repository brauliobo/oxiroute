#[allow(clippy::wildcard_imports)]
use super::*;
use crate::upstream_peer::{POOL_UNAVAILABLE_HEADER, UpstreamPlan};

pub(super) fn h3_retry_trigger(error: &H3UpstreamError) -> HttpRetryTrigger {
    match error {
        H3UpstreamError::Timeout => HttpRetryTrigger::ConnectTimeout,
        H3UpstreamError::RefusedStream => HttpRetryTrigger::RefusedStream,
        H3UpstreamError::ResponseTimeout => HttpRetryTrigger::ResponseTimeout,
        H3UpstreamError::EmptyResponse => HttpRetryTrigger::EmptyResponse,
        H3UpstreamError::JunkResponse => HttpRetryTrigger::JunkResponse,
        _ => HttpRetryTrigger::ConnectFailure,
    }
}

pub(super) fn h3_passive_failure(error: &H3UpstreamError) -> Option<HealthFailure> {
    match error {
        H3UpstreamError::Connect => Some(HealthFailure::ConnectFailed),
        H3UpstreamError::Timeout | H3UpstreamError::ResponseTimeout => Some(HealthFailure::Timeout),
        H3UpstreamError::RefusedStream
        | H3UpstreamError::Protocol
        | H3UpstreamError::EmptyResponse
        | H3UpstreamError::JunkResponse
        | H3UpstreamError::Failed => Some(HealthFailure::ProtocolError),
        H3UpstreamError::QueueTimeout => Some(HealthFailure::UnexpectedStatus),
        H3UpstreamError::Cancelled
        | H3UpstreamError::ResponseBodyTooLarge
        | H3UpstreamError::RequestBodyTooLarge
        | H3UpstreamError::ResourceExhausted
        | H3UpstreamError::MissingServerName
        | H3UpstreamError::PoolUnavailable => None,
    }
}

fn h3_failure_status(error: &H3UpstreamError) -> StatusCode {
    match error {
        H3UpstreamError::Timeout | H3UpstreamError::ResponseTimeout => StatusCode::GATEWAY_TIMEOUT,
        H3UpstreamError::PoolUnavailable | H3UpstreamError::QueueTimeout => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Answers a request whose upstream exchange failed: a pool without a selectable server says so with the marker
/// header and the pool's own 503 body, the other failures with a bare status.
pub(super) async fn send_h3_upstream_failure(
    stream: &mut H3Stream,
    error: &H3UpstreamError,
    pool: &UpstreamPlan,
    head: bool,
    deadline: Instant,
) -> Option<u16> {
    let status = h3_failure_status(error);
    let sent = if matches!(error, H3UpstreamError::PoolUnavailable) {
        let unavailable = pool.unavailable_response();
        let mut headers = vec![(POOL_UNAVAILABLE_HEADER, HeaderValue::from_static("1"))];
        headers.extend(
            (unavailable.retry_after.clone()).map(|value| (http::header::RETRY_AFTER, value)),
        );
        if !unavailable.body.is_empty() {
            let plain = HeaderValue::from_static("text/plain; charset=utf-8");
            headers.push((http::header::CONTENT_TYPE, plain));
        }
        send_h3_response(
            stream,
            status,
            &headers,
            unavailable.body.clone(),
            head,
            deadline,
        )
        .await
    } else {
        send_h3_error(
            stream,
            status,
            b"HTTP/3 upstream request failed\n",
            deadline,
        )
        .await
    };
    sent.then_some(status.as_u16())
}
