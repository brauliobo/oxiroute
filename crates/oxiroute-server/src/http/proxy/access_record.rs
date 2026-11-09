use std::{
    borrow::Cow,
    time::{SystemTime, UNIX_EPOCH},
};

use pingora::proxy::Session;

use super::{HttpActionPlan, HttpRequestContext, normalized_request_host};
use crate::{
    http::request_id::{REQUEST_ID_HEADER, idempotency_header_or_default, idempotency_tag},
    observability::access_record::{Correlation, Count, HttpAccess, Safe, SafeId},
};

/// The completed request as one access line; text borrows from the context and is redacted on write.
pub(super) fn build<'a>(
    ctx: &'a HttpRequestContext,
    session: &'a Session,
    response_status: Option<u16>,
    elapsed: std::time::Duration,
    service: &'a str,
) -> HttpAccess<'a> {
    let request = session.req_header();
    let request_id = request
        .headers
        .get(&REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok());
    let trace = &ctx.trace;
    HttpAccess {
        attempts: ctx.attempt_count(),
        bytes_received: Count(session.body_bytes_read() as u64),
        bytes_sent: Count(session.body_bytes_sent() as u64),
        connect_ms: trace.connect.map(|duration| Count(duration.as_millis())),
        correlation_id: Correlation(request_id),
        duration_ms: Count(elapsed.as_millis()),
        evacuations: ctx
            .evacuation()
            .map_or(0, |evacuation| evacuation.redispatches()),
        header_ms: trace.header.map(|duration| Count(duration.as_millis())),
        host: ctx
            .authority
            .as_ref()
            .map(|host| normalized_request_host(host).into()),
        idem_key: idempotency_key(ctx, session).map(Safe::from),
        method: request.method.as_str().into(),
        pool: ctx.pool.as_ref().map(|pool| pool.selector().name().into()),
        protocol: SafeId(if session.is_http2() { "h2" } else { "http/1.1" }),
        queue_wait_ms: Count(trace.queue_wait.as_millis()),
        reason: trace.failure_reason.unwrap_or("").into(),
        request_id: request_id.map(Safe::from),
        rescued: ctx.rescued,
        route: ctx
            .route
            .as_ref()
            .map(|route| route.route_id.as_str().into()),
        server: trace.servers.last().map(|server| server.as_str().into()),
        servers: tried_servers(&trace.servers).map(Safe::from),
        service: service.into(),
        status: response_status,
        timestamp_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        was_stuck: ctx
            .inflight
            .as_ref()
            .is_some_and(crate::http::inflight::InflightGuard::stuck),
    }
}

fn idempotency_key(ctx: &HttpRequestContext, session: &Session) -> Option<String> {
    let HttpActionPlan::Proxy(proxy) = &ctx.route.as_ref()?.action else {
        return None;
    };
    let name = idempotency_header_or_default(proxy.policy.idempotency_header.as_ref());
    session
        .req_header()
        .headers
        .get(name)
        .map(|value| idempotency_tag(value.as_bytes()))
}

/// Every server tried, comma separated; the single-attempt case borrows.
fn tried_servers(servers: &[String]) -> Option<Cow<'_, str>> {
    match servers {
        [] => None,
        [only] => Some(Cow::Borrowed(only)),
        many => Some(Cow::Owned(many.join(","))),
    }
}
