//! The access line of a request the lane served. It is the classic record, `HttpAccess`, written by the lane without
//! building one: the text that is the same for every request of a route or a service (pool, route and service names)
//! is escaped once with the plan, text that is known clean is not searched for credentials again, and the line goes
//! straight to the sink's buffer. `HttpAccess::write_json` is the specification of the bytes; the lane's differential
//! tests compare the two for a corpus of requests.

use std::{cell::RefCell, time::Duration};

use http::uri::Authority;
use log::warn;

use super::proxy::{Forward, Progress};
use crate::{
    http::{
        action::AccessLog,
        policy::normalized_request_host,
        proxy::{
            HttpRequestContext,
            access_record::{idempotency_header, method_text},
        },
        request_id::idempotency_tag,
    },
    label::Label,
    observability::{
        access_record::{push_decimal, push_safe, push_text, push_u64},
        logging::redact_text_cow,
    },
};

/// The names of a route that its access lines repeat, as the line writes them: from `,"pool":` to the opening quote of
/// the queue wait, and the whole `route` field.
#[derive(Debug, Default)]
pub(crate) struct RouteLog {
    pool: Box<[u8]>,
    route: Box<[u8]>,
}

impl RouteLog {
    pub(crate) fn new(pool: &Label, route: &Label) -> Self {
        let mut pool_part = br#","pool":"#.to_vec();
        push_text(&mut pool_part, pool.logged());
        pool_part.extend_from_slice(br#","protocol":"http/1.1","queueWaitMs":""#);
        let mut route_part = br#","route":"#.to_vec();
        push_text(&mut route_part, route.logged());
        Self {
            pool: pool_part.into(),
            route: route_part.into(),
        }
    }
}

thread_local! {
    static HOSTS: RefCell<[Option<(Box<str>, Box<[u8]>)>; HOST_SLOTS]> =
        const { RefCell::new([const { None }; HOST_SLOTS]) };
}

const HOST_SLOTS: usize = 8;
/// A longer host is not remembered.
const HOST_REMEMBERED_BYTES: usize = 64;

/// The `host` field as the line writes it for an authority: normalised, redacted and escaped. Which text it is has
/// nothing to do with the request, so the few hosts a thread serves are worked out once.
fn push_host(out: &mut Vec<u8>, host: &Authority) {
    let text = host.as_str();
    let bytes = text.as_bytes();
    let slot = (bytes.len() ^ usize::from(bytes[0]) ^ usize::from(bytes[bytes.len() - 1]) << 2)
        % HOST_SLOTS;
    HOSTS.with_borrow_mut(|slots| {
        if let Some((known, field)) = &slots[slot]
            && **known == *text
        {
            out.extend_from_slice(field);
            return;
        }
        let start = out.len();
        push_text(out, &redact_text_cow(&normalized_request_host(host)));
        if text.len() <= HOST_REMEMBERED_BYTES {
            slots[slot] = Some((text.into(), out[start..].into()));
        }
    });
}

/// `,"service":` and the service name as the line writes it.
pub(super) fn service_part(log: &AccessLog) -> Box<[u8]> {
    let mut part = br#","service":"#.to_vec();
    push_text(&mut part, log.logged_service());
    part.into()
}

pub(super) fn write(
    log: &AccessLog,
    service: &[u8],
    ctx: &HttpRequestContext,
    forward: &Forward<'_>,
    progress: &Progress,
    elapsed: Duration,
) {
    let result = log.write_with(|line| {
        render(line, service, ctx, forward, progress, elapsed);
        Ok(())
    });
    if let Err(error) = result {
        warn!("HTTP access log write failed: {error}");
    }
}

/// Appends a duration in milliseconds as a decimal string, or `null`.
fn millis(out: &mut Vec<u8>, prefix: &[u8], duration: Option<Duration>) {
    out.extend_from_slice(prefix);
    match duration {
        Some(duration) => {
            out.push(b'"');
            push_decimal(out, duration.as_millis());
            out.push(b'"');
        }
        None => out.extend_from_slice(b"null"),
    }
}

fn boolean(out: &mut Vec<u8>, prefix: &[u8], value: bool) {
    out.extend_from_slice(prefix);
    out.extend_from_slice(if value { b"true" } else { b"false" });
}

fn render(
    out: &mut Vec<u8>,
    service: &[u8],
    ctx: &HttpRequestContext,
    forward: &Forward<'_>,
    progress: &Progress,
    elapsed: Duration,
) {
    let (buffer, head) = (forward.buffer, forward.head);
    let trace = &ctx.trace;
    let route = &forward.route.edits.log;
    out.reserve(512);
    out.extend_from_slice(br#"{"attempts":"#);
    push_u64(out, ctx.attempt_count() as u64);
    out.extend_from_slice(br#","bytesReceived":""#);
    push_u64(out, progress.received as u64);
    out.extend_from_slice(br#"","bytesSent":""#);
    push_u64(out, progress.sent as u64);
    out.push(b'"');
    millis(out, br#","connectMs":"#, trace.connect);
    // The id is a valid one by construction: a client's only when it passed the check, else the proxy's own.
    out.extend_from_slice(br#","correlationId":""#);
    out.extend_from_slice(forward.id.as_bytes());
    out.extend_from_slice(br#"","durationMs":""#);
    push_decimal(out, elapsed.as_millis());
    out.extend_from_slice(br#"","evacuations":"#);
    let redispatches = ctx
        .evacuation()
        .map_or(0, |evacuation| evacuation.redispatches());
    push_u64(out, redispatches as u64);
    millis(out, br#","headerMs":"#, trace.header);
    out.extend_from_slice(br#","host":"#);
    match &ctx.authority {
        Some(host) if !host.as_str().is_empty() => push_host(out, host),
        Some(host) => push_text(out, &redact_text_cow(&normalized_request_host(host))),
        None => out.extend_from_slice(b"null"),
    }
    out.extend_from_slice(br#","idemKey":"#);
    let idempotency = idempotency_header(ctx).and_then(|name| {
        let name = name.as_str().as_bytes();
        head.headers()
            .iter()
            .find(|header| header.name(buffer).eq_ignore_ascii_case(name))
            .map(|header| header.value(buffer))
    });
    match idempotency {
        Some(key) => push_text(out, &redact_text_cow(&idempotency_tag(key))),
        None => out.extend_from_slice(b"null"),
    }
    out.extend_from_slice(br#","method":"#);
    push_safe(out, &method_text(forward.method));
    out.extend_from_slice(&route.pool);
    push_decimal(out, trace.queue_wait.as_millis());
    out.extend_from_slice(br#"","reason":"#);
    push_text(out, trace.failure_reason.unwrap_or(""));
    out.extend_from_slice(br#","requestId":"#);
    if forward.request.client_id.is_none() {
        // A generated id is hex between fixed letters and dashes: no credential marker can match it.
        out.push(b'"');
        out.extend_from_slice(forward.id.as_bytes());
        out.push(b'"');
    } else {
        let id = String::from_utf8_lossy(forward.id.as_bytes());
        push_text(out, &redact_text_cow(&id));
    }
    boolean(out, br#","rescued":"#, ctx.rescued);
    out.extend_from_slice(&route.route);
    out.extend_from_slice(br#","server":"#);
    match trace.servers.last() {
        Some(server) => push_text(out, server.logged()),
        None => out.extend_from_slice(b"null"),
    }
    out.extend_from_slice(br#","servers":"#);
    match trace.servers.as_slice() {
        [] => out.extend_from_slice(b"null"),
        [only] => push_text(out, only.logged()),
        many => {
            let names: Vec<&str> = many.iter().map(|server| &**server).collect();
            push_text(out, &redact_text_cow(&names.join(",")));
        }
    }
    out.extend_from_slice(service);
    out.extend_from_slice(br#","status":"#);
    match progress.status {
        Some(status) => push_u64(out, u64::from(status)),
        None => out.extend_from_slice(b"null"),
    }
    out.extend_from_slice(br#","timestampUnixMs":"#);
    push_u64(out, crate::clock::wall_ms(ctx.now));
    let stuck = ctx
        .inflight
        .as_ref()
        .is_some_and(crate::http::inflight::InflightGuard::stuck);
    boolean(out, br#","wasStuck":"#, stuck);
    out.push(b'}');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classic(host: &Authority) -> Vec<u8> {
        let mut out = Vec::new();
        push_text(&mut out, &redact_text_cow(&normalized_request_host(host)));
        out
    }

    /// A host is written the same on its first appearance, from the memory of the thread, and after other hosts that
    /// share its place in it.
    #[test]
    fn a_remembered_host_is_written_as_the_classic_line_writes_it() {
        let hosts: Vec<Authority> = [
            "example.test",
            "Token.Example.Test:8080",
            "[2001:db8::1]:8443",
            "192.0.2.7",
            "EXAMPLE.test.",
            "secret.test",
            &format!("{}.test", "a".repeat(100)),
        ]
        .iter()
        .map(|host| host.parse().expect("an authority"))
        .collect();
        for _ in 0..3 {
            for host in &hosts {
                let mut out = Vec::new();
                push_host(&mut out, host);
                assert_eq!(out, classic(host), "{host}");
            }
        }
        for host in (0..64).map(|index| {
            format!("h{index}.example.test")
                .parse::<Authority>()
                .unwrap()
        }) {
            for _ in 0..2 {
                let mut out = Vec::new();
                push_host(&mut out, &host);
                assert_eq!(out, classic(&host), "{host}");
            }
        }
    }
}
