//! The proxied lane: a request without a body is forwarded to a plain HTTP/1.1 server of a pool on the connection
//! itself. The head goes upstream and the response head comes back through the head codec, without a header map on
//! either side; a response head the codec does not know is read by the classic client parser, so the response is
//! always served. Selection, leases, timeouts, health signals, evacuation and accounting are the classic request
//! context's own steps.

use std::{io::Write as _, sync::Arc, time::Instant};

use bytes::{BufMut as _, Bytes, BytesMut};
use http::{HeaderValue, Method, StatusCode, Version, uri::Authority};
use log::{debug, warn};
use oxiroute_config::HttpUpstreamHost;
use oxiroute_h1_head::{
    Automatic, Class, Connection, Edits, HOP_KEEP_ALIVE, RequestHead, ResponseHead, ScanResponse,
    Value, names, scan_response, write_request, write_response,
};
use pingora::{
    Error, ErrorType,
    apps::Http1Connection,
    connectors::http::v1::Connector,
    protocols::{
        Stream,
        http::{
            date::get_cached_date,
            v1::{
                client::{FastFraming, FastHead, HttpSession},
                server::{render_final_response_head, render_informational_head},
            },
        },
    },
    upstreams::peer::{HttpPeer, Peer as _},
};
use tokio::io::AsyncWriteExt as _;

use super::{
    Served, buffers,
    head::Request,
    timing::{Expired, within},
};
use crate::{
    HealthFailure,
    http::{
        action::{HttpActionPlan, HttpRoutePlan},
        evacuation::LeftRunning,
        inflight::Phase,
        proxy::{
            ErrorReason, HttpRequestContext, apply_response_policy, classify_http_result,
            error_reason, error_response, failure_reason, proxy_error_status, proxy_policy,
            selected_upstream_host,
        },
        request_id::stamp_response,
    },
    upstream_peer::POOL_UNAVAILABLE_HEADER,
};

/// How the body of the response goes to the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Framing {
    Length,
    Chunked,
    Empty,
}

/// An origin response whose head is ready to go to the client.
struct Opened {
    session: HttpSession,
    peer: Box<HttpPeer>,
    head: Vec<u8>,
    framing: Framing,
    status: u16,
    keepalive: bool,
    /// The whole body arrived with the head, so the first read of it does not wait.
    buffered: bool,
}

/// What the request sends to the origin as its `Host`.
enum HostEdit {
    AsSent,
    Set(HeaderValue),
    Remove,
}

pub(super) struct Forward<'a> {
    pub(super) buffer: &'a [u8],
    pub(super) head: &'a RequestHead,
    pub(super) request: &'a Request,
    pub(super) route: &'a Arc<HttpRoutePlan>,
    pub(super) id: &'a HeaderValue,
    /// Whether the connection serves another request after this one, as far as the request knows.
    pub(super) keepalive: bool,
}

impl super::LaneFront {
    pub(super) async fn serve_proxied(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        forward: Forward<'_>,
        authority: Authority,
    ) -> Served {
        let upstream = &*self.upstream;
        let mut ctx = self.begin(stream, &forward, authority);
        let mut sent = 0;
        let mut status = None;
        let outcome = self
            .exchange(
                stream,
                connection,
                &mut ctx,
                upstream,
                &forward,
                &mut sent,
                &mut status,
            )
            .await;
        let error = outcome.err();
        if let Some(error) = &error {
            warn!(
                "HTTP request failed with {:?} from {:?}",
                error.etype(),
                error.esource()
            );
            status = self
                .fail(stream, &mut ctx, error, &forward, &mut sent, status)
                .await;
        }
        self.finish(&mut ctx, error.as_deref(), status, sent);
        connection.end_request();
        if error.is_none() && connection.will_keepalive() {
            return Served::Next;
        }
        let _ = stream.shutdown().await;
        Served::Close
    }

    /// The request context of a request the lane serves, with the facts the classic request filter establishes.
    fn begin(
        &self,
        stream: &Stream,
        forward: &Forward<'_>,
        authority: Authority,
    ) -> HttpRequestContext {
        let (buffer, request, route) = (forward.buffer, forward.request, forward.route);
        let HttpActionPlan::Proxy(proxy) = &route.action else {
            unreachable!("only proxied routes reach the proxied lane");
        };
        let mut ctx = HttpRequestContext::new(
            self.metrics.clone(),
            Some(Arc::clone(&self.generation)),
            Instant::now(),
            self.service.upstream_io_timeout(),
        );
        ctx.request_id = Some(forward.id.clone());
        ctx.adopt_client_timeouts([self.timeouts.request, self.timeouts.session_write()]);
        let client_ip = stream.get_socket_digest().and_then(|digest| {
            digest
                .peer_addr()
                .and_then(|address| address.as_inet().map(std::net::SocketAddr::ip))
        });
        let method = Method::from_bytes(request.method(buffer))
            .expect("the route was selected for this method");
        ctx.register_arrival(
            &method,
            request.host(buffer),
            request.path(buffer),
            client_ip,
            None,
        );
        ctx.authority = Some(authority);
        if let Some(inflight) = &ctx.inflight {
            inflight.place_route(route.route_id.shared());
            inflight.place_pool(proxy.pool.label().shared());
        }
        ctx.route = Some(Arc::clone(route));
        let policy = &proxy.policy;
        let replay_safe = policy.request_is_replay_safe(
            &method,
            &http::HeaderMap::new(),
            true,
            route.policy.request_buffering,
        );
        ctx.evacuation_pin = LeftRunning::unmovable(false, replay_safe);
        ctx.adopt_route_total(
            policy
                .timeouts
                .total_ms
                .map(std::time::Duration::from_millis),
        );
        ctx.pool = Some(Arc::clone(&proxy.pool));
        ctx
    }

    /// The origin's answer, sent to the client; the status that went out is left in `status`.
    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
        sent: &mut usize,
        status: &mut Option<u16>,
    ) -> Result<(), Box<Error>> {
        let opened = self.attempts(stream, ctx, upstream, forward).await?;
        *status = Some(opened.status);
        if !opened.keepalive {
            connection.disable_keepalive();
        }
        self.relay(stream, ctx, upstream, opened, sent)
            .await
            .map_err(|error| ctx.attempt_failed_for_lane(error))
    }

    /// Attempts on the pool's servers until one answers or the request fails; an attempt an operator moved goes on
    /// to another server.
    async fn attempts(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
    ) -> Result<Opened, Box<Error>> {
        loop {
            let cancelled = ctx.arm_evacuation_for_lane();
            let outcome = {
                let attempt = self.attempt(stream, ctx, upstream, forward);
                tokio::select! {
                    biased;
                    error = cancelled => Err(ctx.attempt_failed_for_lane(error)),
                    opened = attempt => opened,
                }
            };
            match outcome {
                Ok(opened) => return Ok(opened),
                Err(error) if error.retry() => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn attempt(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
    ) -> Result<Opened, Box<Error>> {
        let peer = ctx
            .next_peer(
                |_| None,
                |endpoint, policy, authority| match policy {
                    HttpUpstreamHost::PreserveIncoming => Ok(None),
                    policy => selected_upstream_host(endpoint, policy, authority),
                },
            )
            .await?;
        let (session, reused) = match upstream.get_http_session(&*peer).await {
            Ok(connected) => connected,
            Err(mut error) => {
                error.as_up();
                return Err(ctx.connect_failed(error).into_up());
            }
        };
        self.send_and_open(stream, ctx, session, reused, peer, forward)
            .await
            .map_err(|error| ctx.attempt_failed_for_lane(error))
    }

    /// Sends the request on a connected session and reads the response head.
    async fn send_and_open(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        mut session: HttpSession,
        reused: bool,
        peer: Box<HttpPeer>,
        forward: &Forward<'_>,
    ) -> Result<Opened, Box<Error>> {
        ctx.connected(reused, None)?;
        session.read_timeout = peer.options.read_timeout;
        session.body_read_timeout = peer.options.body_read_timeout;
        session.write_timeout = peer.options.write_timeout;
        ctx.remaining_cached(true)?;
        let host = host_edit(ctx);
        let connection_close = ctx.pool.as_ref().is_some_and(|pool| {
            pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
        });
        let mut request = buffers::take_write();
        render_request(&mut request, forward, &host, connection_close);
        let written = session
            .write_request_head_raw(&request, forward.request.is_head)
            .await;
        buffers::give_write(request);
        written.map_err(Error::into_up)?;
        self.open(stream, ctx, session, peer, forward).await
    }

    /// Reads the response head, applies what the classic response filter applies and renders what the client gets.
    async fn open(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        mut session: HttpSession,
        peer: Box<HttpPeer>,
        forward: &Forward<'_>,
    ) -> Result<Opened, Box<Error>> {
        let raw = session.read_raw_head().await.map_err(Error::into_up)?;
        let keepalive = forward.keepalive && !self.generation.retired();
        let mut head = buffers::take_write();
        let (status, framing, refused, keepalive, buffered) =
            if let Some((status, framing, refused, buffered)) =
                serve_fast(&mut head, &raw, &mut session, forward, keepalive)
            {
                (status, framing, refused, keepalive, buffered)
            } else {
                session.reparse_raw_head();
                let mut response = read_final_head(stream, ctx, &mut session, forward).await?;
                let refused = response.status == StatusCode::SERVICE_UNAVAILABLE
                    && response.headers.contains_key(&POOL_UNAVAILABLE_HEADER);
                let (framing, close_delimited) =
                    prepare_slow(ctx, &mut session, &mut response, forward)?;
                let status = response.status.as_u16();
                let keepalive = keepalive && !close_delimited;
                let mut rendered = BytesMut::new();
                render_final_response_head(&mut response, keepalive, &mut rendered)
                    .map_err(Error::into_up)?;
                head.extend_from_slice(&rendered);
                (status, framing, refused, keepalive, false)
            };
        let now = ctx.refresh_clock();
        ctx.remaining_at(now, true)?;
        ctx.commit_response(StatusCode::from_u16(status).expect("a scanned status"))?;
        if refused {
            ctx.record_passive_failure(HealthFailure::UnexpectedStatus);
        } else {
            ctx.record_passive_success();
        }
        ctx.note_response(status, now);
        ctx.enter_phase(Phase::Streaming, now);
        Ok(Opened {
            session,
            peer,
            head,
            framing,
            status,
            keepalive,
            buffered,
        })
    }

    /// Writes the response head and relays the body, then gives the origin connection back to the pool.
    async fn relay(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        opened: Opened,
        sent: &mut usize,
    ) -> Result<(), Box<Error>> {
        let Opened {
            mut session,
            peer,
            head,
            framing,
            buffered,
            ..
        } = opened;
        let deadline = tokio::time::Instant::from_std(ctx.deadline);
        let body = async {
            // `frame` carries the head and then each piece of the body with its chunk framing. `sent` counts the
            // body, as a session does.
            let mut frame = head;
            if !buffered {
                write(stream, &frame).await?;
                flush(stream).await?;
                frame.clear();
            }
            let mut first = true;
            while let Some(data) = session.read_body_bytes().await.map_err(Error::into_up)? {
                *sent += data.len();
                // The first piece follows the clock reading of the head.
                let now = if std::mem::take(&mut first) {
                    ctx.now
                } else {
                    Instant::now()
                };
                ctx.observe_bytes(0, *sent, now);
                if framing == Framing::Chunked {
                    write!(frame, "{:x}\r\n", data.len()).expect("a buffer grows");
                    frame.extend_from_slice(&data);
                    frame.extend_from_slice(b"\r\n");
                } else {
                    frame.extend_from_slice(&data);
                }
                write(stream, &frame).await?;
                flush(stream).await?;
                frame.clear();
            }
            if !frame.is_empty() {
                write(stream, &frame).await?;
                frame.clear();
            }
            if framing == Framing::Chunked {
                write(stream, b"0\r\n\r\n").await?;
            }
            buffers::give_write(frame);
            flush(stream).await
        };
        within(deadline, body)
            .await
            .map_err(|Expired| Error::new_up(ErrorType::ReadTimedout))??;
        upstream
            .release_http_session(session, &*peer, peer.idle_timeout())
            .await;
        Ok(())
    }

    /// Answers a request that ended in `error`, unless a response went out already; the status of the response
    /// that the client got, if any.
    async fn fail(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        error: &Error,
        forward: &Forward<'_>,
        sent: &mut usize,
        sent_status: Option<u16>,
    ) -> Option<u16> {
        let started = sent_status.is_some();
        ctx.trace.failure_reason = Some(failure_reason(error, ctx, started));
        if started || ctx.client_deadline_expired() {
            return sent_status;
        }
        let reason = error_reason(error, ctx, false);
        let code = reason.map_or_else(|| proxy_error_status(error), ErrorReason::status);
        if let Some(reason) = reason {
            debug!("request ended by {}", reason.as_str());
        }
        if code == 0 {
            return None;
        }
        let rendered = error_response(
            code,
            error,
            reason,
            ctx.pool.as_deref(),
            Some(forward.id),
            forward.request.is_head,
        )
        .and_then(|(mut response, body)| {
            let mut head = BytesMut::new();
            render_final_response_head(&mut response, false, &mut head)?;
            head.put_slice(&body);
            Ok((head, body.len()))
        });
        match rendered {
            Ok((response, body)) => match write(stream, &response).await {
                Ok(()) => {
                    *sent += body;
                    Some(code)
                }
                Err(write_error) => {
                    warn!("failed to send error response downstream: {write_error}");
                    None
                }
            },
            Err(render_error) => {
                warn!("failed to send error response downstream: {render_error}");
                None
            }
        }
    }

    /// What the classic `logging` hook accounts for when a request ends.
    fn finish(
        &self,
        ctx: &mut HttpRequestContext,
        error: Option<&Error>,
        status: Option<u16>,
        sent: usize,
    ) {
        let now = ctx.refresh_clock();
        let elapsed = now.saturating_duration_since(ctx.started_at);
        let result = classify_http_result(error, status);
        ctx.operation_result = Some(result);
        if let Err(metric_error) = self.metrics.record_http_operation(result, elapsed) {
            warn!("could not account for HTTP operation metrics: {metric_error}");
        }
        ctx.observe_bytes(0, sent, now);
        ctx.release_lease();
        ctx.account_attempt(0, sent, now);
        ctx.record_failed_evacuations(error);
    }
}

/// The stream buffers its writes: what a client waits for must be flushed before the next wait on the origin.
async fn flush(stream: &mut Stream) -> Result<(), Box<Error>> {
    stream.flush().await.map_err(|error| {
        Error::because(ErrorType::WriteError, "flushing the response", error).into_down()
    })
}

async fn write(stream: &mut Stream, bytes: &[u8]) -> Result<(), Box<Error>> {
    stream.write_all(bytes).await.map_err(|error| {
        Error::because(ErrorType::WriteError, "writing the response", error).into_down()
    })
}

fn host_edit(ctx: &mut HttpRequestContext) -> HostEdit {
    if matches!(
        proxy_policy(ctx).upstream_host,
        HttpUpstreamHost::PreserveIncoming
    ) {
        return HostEdit::AsSent;
    }
    ctx.selected_upstream_host
        .take()
        .map_or(HostEdit::Remove, HostEdit::Set)
}

/// Whether the response head is one the codec serves: final, no upgrade, a plain `Connection` and no `Keep-Alive`.
fn is_plain(head: &ResponseHead) -> bool {
    let facts = head.facts;
    !facts.informational
        && !facts.upgrade
        && facts.connection != Connection::Other
        && facts.hop_by_hop & HOP_KEEP_ALIVE == 0
        && (!facts.transfer_encoding || facts.chunked_final)
}

fn refused_by_pool(buffer: &[u8], head: &ResponseHead) -> bool {
    head.status == 503
        && head.headers().iter().any(|header| {
            header
                .name(buffer)
                .eq_ignore_ascii_case(POOL_UNAVAILABLE_HEADER.as_str().as_bytes())
        })
}

/// The upstream request head: the client's, as the classic path forwards it. The edits are the `HeaderMap` calls of
/// the classic sequence in its order: the `Connection` of a keep-alive client leaves with the hop-by-hop fields, an
/// id is stamped when the request has no single valid one, the host policy applies, a client's `close` is not
/// forwarded and a pool that never reuses connections says `close` itself.
fn render_request(
    out: &mut Vec<u8>,
    forward: &Forward<'_>,
    host: &HostEdit,
    connection_close: bool,
) {
    let (buffer, head, request) = (forward.buffer, forward.head, forward.request);
    let connection = head.facts.connection;
    let mut edits = Edits::new();
    let edit = |result: Result<(), oxiroute_h1_head::EditsFull>| {
        result.expect("a request head needs few edits");
    };
    if connection == Connection::KeepAlive {
        edit(edits.remove(names::CONNECTION));
    }
    if !request.id_stamped {
        edit(edits.set(names::X_REQUEST_ID, Value::trusted(forward.id.as_bytes())));
    }
    match host {
        HostEdit::AsSent => {}
        HostEdit::Set(value) => edit(edits.set(names::HOST, Value::trusted(value.as_bytes()))),
        HostEdit::Remove => edit(edits.remove(names::HOST)),
    }
    if connection == Connection::Close {
        edit(edits.remove(names::CONNECTION));
    }
    if connection_close {
        edit(edits.set(names::CONNECTION, Value::trusted(b"close")));
    }
    write_request(out, buffer, head, &edits);
}

/// Serves a response head the codec scans and knows: the status, the framing and whether the origin refused the
/// request as a pool without servers. `None` leaves the head to the classic parser.
fn serve_fast(
    out: &mut Vec<u8>,
    raw: &Bytes,
    session: &mut HttpSession,
    forward: &Forward<'_>,
    keepalive: bool,
) -> Option<(u16, Framing, bool, bool)> {
    let mut head = buffers::take_response_head();
    let served =
        (scan_response(raw, &mut head) == ScanResponse::Head && is_plain(&head)).then(|| {
            let framing = render_fast(out, raw, &head, session, forward, keepalive);
            let body = &raw[head.head_len as usize..];
            let buffered = match framing {
                Framing::Length => head
                    .facts
                    .content_length
                    .is_some_and(|length| body.len() as u64 >= length),
                Framing::Chunked => body.ends_with(b"0\r\n\r\n"),
                Framing::Empty => false,
            };
            (head.status, framing, refused_by_pool(raw, &head), buffered)
        });
    buffers::give_response_head(head);
    served
}

/// The client's response head for a head the codec scanned, and the facts of it reported to the session.
fn render_fast(
    out: &mut Vec<u8>,
    raw: &Bytes,
    head: &ResponseHead,
    session: &mut HttpSession,
    forward: &Forward<'_>,
    keepalive: bool,
) -> Framing {
    let facts = head.facts;
    let is_head = forward.request.is_head;
    let mut edits = Edits::new();
    let edit = |result: Result<(), oxiroute_h1_head::EditsFull>| {
        result.expect("a response head needs few edits");
    };
    if facts.transfer_encoding && head.find(Class::ContentLength).is_some() {
        edit(edits.remove(names::CONTENT_LENGTH));
    }
    for (class, name) in [
        (Class::Connection, names::CONNECTION),
        (Class::KeepAlive, names::KEEP_ALIVE),
        (Class::ProxyAuthenticate, names::PROXY_AUTHENTICATE),
        (Class::ProxyAuthorization, names::PROXY_AUTHORIZATION),
        (Class::ProxyConnection, names::PROXY_CONNECTION),
        (Class::Te, names::TE),
        (Class::Trailer, names::TRAILER),
        (Class::TransferEncoding, names::TRANSFER_ENCODING),
        (Class::Upgrade, names::UPGRADE),
    ] {
        if facts.present & class.bit() != 0 {
            edit(edits.remove(name));
        }
    }
    let no_body = is_head || matches!(head.status, 204 | 304);
    let chunked = !no_body && facts.content_length.is_none();
    edit(edits.stamp_request_id(raw, head, Value::trusted(forward.id.as_bytes())));
    if chunked {
        edit(edits.set(names::TRANSFER_ENCODING, Value::trusted(b"chunked")));
    }
    let date = get_cached_date();
    let automatic = Automatic {
        date: date.as_bytes(),
        connection: if keepalive { b"keep-alive" } else { b"close" },
    };
    let minor = if chunked { 1 } else { head.minor };
    write_response(out, raw, head, &edits, minor, Some(automatic));
    session.accept_fast_head(FastHead {
        status: StatusCode::from_u16(head.status).expect("a scanned status"),
        version: if head.minor == 0 {
            Version::HTTP_10
        } else {
            Version::HTTP_11
        },
        head_len: head.head_len as usize,
        connection: match facts.connection {
            Connection::KeepAlive => Some(HeaderValue::from_static("keep-alive")),
            Connection::Close => Some(HeaderValue::from_static("close")),
            Connection::Absent | Connection::Other => None,
        },
        transfer_encoding: facts.transfer_encoding,
        framing: FastFraming {
            chunked: facts.transfer_encoding && facts.chunked_final,
            content_length: facts
                .content_length
                .map(|length| usize::try_from(length).expect("a scanned length")),
        },
    });
    match (no_body, chunked) {
        (true, _) => Framing::Empty,
        (false, true) => Framing::Chunked,
        (false, false) => Framing::Length,
    }
}

/// The final response head of the session's parser; informational heads on the way go to the client filtered and
/// stamped as the classic response filter does.
async fn read_final_head(
    stream: &mut Stream,
    ctx: &HttpRequestContext,
    session: &mut HttpSession,
    forward: &Forward<'_>,
) -> Result<pingora::http::ResponseHeader, Box<Error>> {
    loop {
        let mut header = *session
            .read_resp_header_parts()
            .await
            .map_err(Error::into_up)?;
        if header.status == StatusCode::SWITCHING_PROTOCOLS {
            return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
        }
        if !header.status.is_informational() {
            return Ok(header);
        }
        apply_response_policy(&mut header, proxy_policy(ctx), true)?;
        stamp_response(&mut header, forward.id)?;
        let mut head = BytesMut::new();
        render_informational_head(&header, &mut head).map_err(Error::into_up)?;
        write(stream, &head).await?;
        flush(stream).await?;
    }
}

/// What the classic response filter and the proxy do to a head the classic parser read. The framing, and whether the
/// body is delimited by the end of the connection.
fn prepare_slow(
    ctx: &mut HttpRequestContext,
    session: &mut HttpSession,
    response: &mut pingora::http::ResponseHeader,
    forward: &Forward<'_>,
) -> Result<(Framing, bool), Box<Error>> {
    apply_response_policy(response, proxy_policy(ctx), true)?;
    stamp_response(response, forward.id)?;
    let no_body = forward.request.is_head || matches!(response.status.as_u16(), 204 | 304);
    let has_length = response.headers.contains_key(http::header::CONTENT_LENGTH);
    let has_coding = response
        .headers
        .contains_key(http::header::TRANSFER_ENCODING);
    if !no_body && !has_length && !has_coding && !session.is_body_done() {
        response.set_version(Version::HTTP_11);
        response.insert_header(http::header::TRANSFER_ENCODING, "chunked")?;
        return Ok((Framing::Chunked, false));
    }
    Ok(match (no_body, has_coding, has_length) {
        (true, _, _) => (Framing::Empty, false),
        (false, true, _) => (Framing::Chunked, false),
        (false, false, true) => (Framing::Length, false),
        (false, false, false) => (Framing::Empty, true),
    })
}
