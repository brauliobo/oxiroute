use std::borrow::Cow;

use std::sync::Arc;

use pingora::proxy::Session;

use http::Method;

use super::{HttpActionPlan, HttpRequestContext, normalized_request_host};
use crate::{
    http::request_id::{idempotency_header_or_default, idempotency_tag},
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
    let request_id = ctx
        .request_id
        .as_ref()
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
        method: method_text(&request.method),
        pool: ctx
            .pool
            .as_ref()
            .map(|pool| Safe::verified(pool.label().logged())),
        protocol: SafeId(if session.is_http2() { "h2" } else { "http/1.1" }),
        queue_wait_ms: Count(trace.queue_wait.as_millis()),
        reason: Safe::verified(trace.failure_reason.unwrap_or("")),
        request_id: request_id.map(Safe::from),
        rescued: ctx.rescued,
        route: ctx
            .route
            .as_ref()
            .map(|route| Safe::verified(route.route_id.logged())),
        server: trace.servers.last().map(|server| (&**server).into()),
        servers: tried_servers(&trace.servers).map(Safe::from),
        service: Safe::verified(service),
        status: response_status,
        timestamp_unix_ms: Some(crate::clock::wall_ms(ctx.now)),
        was_stuck: ctx
            .inflight
            .as_ref()
            .is_some_and(crate::http::inflight::InflightGuard::stuck),
    }
}

/// The standard methods are fixed clean text; an extension method is the client's.
fn method_text(method: &Method) -> Safe<'_> {
    match *method {
        Method::GET
        | Method::HEAD
        | Method::POST
        | Method::PUT
        | Method::DELETE
        | Method::CONNECT
        | Method::OPTIONS
        | Method::TRACE
        | Method::PATCH => Safe::verified(method.as_str()),
        _ => method.as_str().into(),
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
fn tried_servers(servers: &[Arc<str>]) -> Option<Cow<'_, str>> {
    match servers {
        [] => None,
        [only] => Some(Cow::Borrowed(&**only)),
        many => Some(Cow::Owned(many.join(","))),
    }
}
