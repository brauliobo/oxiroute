#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn connect_retry_trigger(error: &Error) -> Option<HttpRetryTrigger> {
    match error.etype() {
        ErrorType::ConnectTimedout => Some(HttpRetryTrigger::ConnectTimeout),
        ErrorType::ConnectRefused | ErrorType::ConnectNoRoute | ErrorType::ConnectError => {
            Some(HttpRetryTrigger::ConnectFailure)
        }
        _ => None,
    }
}

/// A refused request was never forwarded, so it may move to the next server whatever its method or
/// body, within the retry budget. A rescued request always moves to another server.
pub(super) fn retry_target_for(ctx: &HttpRequestContext, refused: bool) -> HttpRetryTarget {
    if refused || ctx.rescued {
        HttpRetryTarget::NextServer
    } else {
        proxy_policy(ctx).target_for_retry(ctx.attempt_count())
    }
}

pub(super) fn response_status_retryable(
    session: &Session,
    ctx: &HttpRequestContext,
    refused: bool,
    rescued: bool,
) -> bool {
    let eligible = if refused {
        ctx.connection_retryable
    } else {
        ctx.replay_retryable
    };
    if !eligible
        || !request_body_replayable(session)
        || session.was_upgraded()
        || !response_is_retryable(session)
    {
        return false;
    }
    let policy = proxy_policy(ctx);
    let has_budget = ctx.attempt_count() <= usize::from(policy.max_retries);
    let retry_target = retry_target_for(ctx, refused || rescued);
    let target_available = match (retry_target, rescue_pool_to_enter(ctx, rescued)) {
        (_, Some(rescue_pool)) => rescue_pool.has_unattempted(&[]),
        (HttpRetryTarget::SameServer, None) => ctx.attempted_upstreams.last().is_some(),
        (HttpRetryTarget::NextServer, None) => ctx
            .pool
            .as_ref()
            .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
    };
    has_budget && target_available && ctx.remaining(true).is_ok()
}

/// The rescue pool a rescued request is about to move to, until it has moved.
pub(super) fn rescue_pool_to_enter(
    ctx: &HttpRequestContext,
    rescued: bool,
) -> Option<&Arc<UpstreamPlan>> {
    let rescue_pool = proxy_action(ctx).rescue_pool.as_ref()?;
    (rescued
        && !ctx
            .pool
            .as_ref()
            .is_some_and(|pool| Arc::ptr_eq(pool, rescue_pool)))
    .then_some(rescue_pool)
}

/// A status listed only by `rescue` is a routing decision, not a sign of an unhealthy server.
pub(super) fn is_rescue_only(error: &Error, policy: &ProxyPolicyPlan) -> bool {
    matches!(error.etype(), ErrorType::HTTPStatus(status)
        if policy.rescues_status(*status) && !policy.retries_on_status(*status))
}

pub(super) fn response_is_retryable(session: &Session) -> bool {
    session.body_bytes_sent() == 0
        && session.response_written().is_none_or(|response| {
            response.status.is_informational() && response.status != StatusCode::SWITCHING_PROTOCOLS
        })
}

pub(super) fn request_body_replayable(session: &Session) -> bool {
    session.body_bytes_read() == 0
        || (!session.retry_buffer_truncated() && session.get_retry_buffer().is_some())
}

pub(super) fn retryable_upstream_error(
    error: &Error,
    session: &Session,
    policy: &ProxyPolicyPlan,
) -> bool {
    match error.etype() {
        ErrorType::HTTPStatus(status) => {
            policy.retries_on_status(*status) || policy.rescues_status(*status)
        }
        _ => {
            response_retry_trigger(error, session).is_some_and(|trigger| policy.retries_on(trigger))
        }
    }
}

fn response_retry_trigger(error: &Error, session: &Session) -> Option<HttpRetryTrigger> {
    if is_refused_stream(error) {
        return Some(HttpRetryTrigger::RefusedStream);
    }
    match error.etype() {
        ErrorType::ConnectionClosed | ErrorType::ReadError if response_is_retryable(session) => {
            Some(HttpRetryTrigger::EmptyResponse)
        }
        ErrorType::ReadTimedout => Some(HttpRetryTrigger::ResponseTimeout),
        ErrorType::InvalidHTTPHeader
        | ErrorType::H1Error
        | ErrorType::H2Error
        | ErrorType::InvalidH2 => Some(HttpRetryTrigger::JunkResponse),
        _ => None,
    }
}

pub(super) fn passive_failure_for_error(error: &Error) -> Option<HealthFailure> {
    (error.esource() == &pingora::ErrorSource::Upstream).then(|| match error.etype() {
        ErrorType::ConnectTimedout
        | ErrorType::TLSHandshakeTimedout
        | ErrorType::ReadTimedout
        | ErrorType::WriteTimedout => HealthFailure::Timeout,
        ErrorType::ConnectRefused | ErrorType::ConnectNoRoute | ErrorType::ConnectError => {
            HealthFailure::ConnectFailed
        }
        ErrorType::HTTPStatus(_) => HealthFailure::UnexpectedStatus,
        _ if is_pool_unavailable(error) => HealthFailure::UnexpectedStatus,
        _ => HealthFailure::ProtocolError,
    })
}

pub(super) const fn should_retry_connection(
    has_address_fallback: bool,
    route_retry_allowed: bool,
) -> bool {
    has_address_fallback || route_retry_allowed
}

fn is_refused_stream(error: &Error) -> bool {
    error
        .root_cause()
        .downcast_ref::<h2::Error>()
        .and_then(h2::Error::reason)
        == Some(h2::Reason::REFUSED_STREAM)
}
