use pingora::proxy::Session;

use super::{HttpRequestContext, normalized_request_host};
use crate::request_id::{REQUEST_ID_HEADER, idempotency_header_or_default, idempotency_tag};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn millis(duration: Duration) -> String {
    duration.as_millis().to_string()
}

/// One NDJSON access line. Large counters are strings, like the rest of the log.
pub(super) fn build(
    ctx: &HttpRequestContext,
    session: &Session,
    response_status: Option<u16>,
    service: &str,
) -> serde_json::Value {
    let request = session.req_header();
    let request_id = request
        .headers
        .get(&REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok());
    let idempotency = ctx.route.as_ref().and_then(|route| match &route.action {
        super::HttpActionPlan::Proxy(proxy) => Some(idempotency_header_or_default(
            proxy.policy.idempotency_header.as_ref(),
        )),
        _ => None,
    });
    let idem_key = idempotency
        .and_then(|name| request.headers.get(name))
        .map(|value| idempotency_tag(value.as_bytes()));
    let trace = &ctx.trace;
    let servers = trace.servers.join(",");
    serde_json::json!({
        "timestampUnixMs": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        "correlationId": request_id,
        "requestId": request_id,
        "idemKey": idem_key,
        "service": service,
        "route": ctx.route.as_ref().map(|route| route.route_id.as_str()),
        "host": ctx.authority.as_ref().map(normalized_request_host),
        "method": request.method.as_str(),
        "protocol": if session.is_http2() { "h2" } else { "http/1.1" },
        "status": response_status,
        "reason": trace.failure_reason.unwrap_or(""),
        "pool": ctx.pool.as_ref().map(|pool| pool.selector().name()),
        "server": trace.servers.last(),
        "servers": (!servers.is_empty()).then_some(servers),
        "attempts": ctx.attempt_count(),
        "rescued": ctx.rescued,
        "wasStuck": ctx.inflight.as_ref().is_some_and(crate::inflight::InflightGuard::stuck),
        "queueWaitMs": millis(trace.queue_wait),
        "connectMs": trace.connect.map(millis),
        "headerMs": trace.header.map(millis),
        "bytesReceived": session.body_bytes_read().to_string(),
        "bytesSent": session.body_bytes_sent().to_string(),
        "durationMs": ctx.started_at.elapsed().as_millis().to_string(),
        "clientIp": session.client_addr().and_then(|address| address.as_inet()).map(|address| address.ip().to_string()),
    })
}
