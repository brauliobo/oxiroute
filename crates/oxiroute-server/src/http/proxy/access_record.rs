use pingora::proxy::Session;

use http::{HeaderName, HeaderValue, Method};

use super::{HttpActionPlan, HttpRequestContext, normalized_request_host};
use crate::label::Label;
use crate::{
    http::request_id::{idempotency_header_or_default, idempotency_tag},
    observability::access_record::{Correlation, Count, HttpAccess, Safe},
};

/// What an access line says of the request beyond its context: the same facts whichever path served it.
pub(super) struct Exchange<'a> {
    pub(super) method: &'a Method,
    /// The value of the request's idempotency header, if it has one.
    pub(super) idempotency: Option<&'a [u8]>,
    pub(super) protocol: &'static str,
    /// The request id is one this proxy generated, which no credential marker can match.
    pub(super) request_id_generated: bool,
    pub(super) bytes_received: u64,
    pub(super) bytes_sent: u64,
}

/// The name of the idempotency header of the request's route, which the access line tags the request with.
pub(super) fn idempotency_header(ctx: &HttpRequestContext) -> Option<HeaderName> {
    let HttpActionPlan::Proxy(proxy) = &ctx.route.as_ref()?.action else {
        return None;
    };
    Some(idempotency_header_or_default(
        proxy.policy.idempotency_header.as_ref(),
    ))
}

/// The completed request of a Pingora session.
pub(super) fn build<'a>(
    ctx: &'a HttpRequestContext,
    session: &'a Session,
    response_status: Option<u16>,
    elapsed: std::time::Duration,
    service: &'a str,
) -> HttpAccess<'a> {
    let request = session.req_header();
    let idempotency = idempotency_header(ctx)
        .and_then(|name| request.headers.get(name))
        .map(HeaderValue::as_bytes);
    let exchange = Exchange {
        method: &request.method,
        idempotency,
        protocol: if session.is_http2() { "h2" } else { "http/1.1" },
        request_id_generated: false,
        bytes_received: session.body_bytes_read() as u64,
        bytes_sent: session.body_bytes_sent() as u64,
    };
    access_line(ctx, &exchange, response_status, elapsed, service)
}

/// The completed request as one access line; text borrows from the context and is redacted on write.
pub(super) fn access_line<'a>(
    ctx: &'a HttpRequestContext,
    exchange: &Exchange<'a>,
    response_status: Option<u16>,
    elapsed: std::time::Duration,
    service: &'a str,
) -> HttpAccess<'a> {
    let request_id = ctx
        .request_id
        .as_ref()
        .and_then(|value| value.to_str().ok());
    let trace = &ctx.trace;
    HttpAccess {
        attempts: ctx.attempt_count(),
        bytes_received: Count(exchange.bytes_received),
        bytes_sent: Count(exchange.bytes_sent),
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
        idem_key: exchange
            .idempotency
            .map(|key| Safe::from(idempotency_tag(key))),
        method: method_text(exchange.method),
        pool: ctx
            .pool
            .as_ref()
            .map(|pool| Safe::verified(pool.label().logged())),
        protocol: Safe::verified(exchange.protocol),
        queue_wait_ms: Count(trace.queue_wait.as_millis()),
        reason: Safe::verified(trace.failure_reason.unwrap_or("")),
        request_id: request_id.map(|id| {
            if exchange.request_id_generated {
                Safe::verified(id)
            } else {
                Safe::from(id)
            }
        }),
        rescued: ctx.rescued,
        route: ctx
            .route
            .as_ref()
            .map(|route| Safe::verified(route.route_id.logged())),
        server: trace
            .servers
            .last()
            .map(|server| Safe::verified(server.logged())),
        servers: tried_servers(&trace.servers),
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
pub(super) fn method_text(method: &Method) -> Safe<'_> {
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

/// Every server tried, comma separated; the single-attempt case is the server's own logged name.
fn tried_servers(servers: &[Label]) -> Option<Safe<'_>> {
    match servers {
        [] => None,
        [only] => Some(Safe::verified(only.logged())),
        many => Some(Safe::from(
            many.iter()
                .map(|server| &**server)
                .collect::<Vec<_>>()
                .join(","),
        )),
    }
}
