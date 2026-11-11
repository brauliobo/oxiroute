//! The proxied lane: a request is forwarded to an HTTP/1.1 server of a pool on the connection itself, with its body
//! when that arrived with the head. The head goes upstream and the response head comes back through the head codec,
//! without a header map on either side; a response head the codec does not know is read by the classic client parser, so the response is
//! always served. Selection, leases, timeouts, health signals, evacuation and accounting are the classic request
//! context's own steps.

use std::{
    io::Write as _,
    sync::Arc,
    time::{Duration, Instant},
};

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
            date::get_cached_date_at,
            v1::{
                client::{FastFraming, FastHead, HttpSession},
                server::{render_final_response_head, render_informational_head},
            },
        },
    },
    upstreams::peer::Peer as _,
};
use tokio::io::AsyncWriteExt as _;

use super::{
    Served, buffers,
    head::Request,
    rules,
    timing::{Expired, within},
};
use crate::{
    HealthFailure,
    clock::wall_ms,
    http::{
        action::{HttpActionPlan, HttpRoutePlan, ProxyPolicyPlan},
        evacuation::LeftRunning,
        inflight::Phase,
        proxy::{
            ErrorReason, HttpRequestContext, apply_response_policy, attempt::AttemptTimeouts,
            classify_http_result, error_reason, error_response, failure_reason, proxy_error_status,
            proxy_policy, selected_upstream_host,
        },
        request_id::stamp_response,
    },
    upstream_peer::{POOL_UNAVAILABLE_HEADER, ReusePeer},
};

/// A body piece of at least this size is written as it was read; smaller ones are copied behind the head.
const DIRECT_BODY: usize = 16 * 1024;

/// How the body of the response goes to the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Framing {
    Length,
    Chunked,
    Empty,
}

/// An origin response whose head is ready to go to the client.
struct Opened {
    // Boxed: the session is 640 bytes and every `.await` that returns it moves it into the caller's future.
    session: Box<HttpSession>,
    /// Where the connection goes back to when the response is served.
    release: ReusePeer,
    head: Vec<u8>,
    framing: Framing,
    status: u16,
    keepalive: bool,
    /// The whole body arrived with the head, so the first read of it does not wait.
    buffered: bool,
    /// The origin connection may serve another request when the response is over.
    reusable: bool,
}

impl Opened {
    /// The response of an origin that did not take the whole request: nothing is reused after it.
    fn unfinished(mut self) -> Self {
        self.keepalive = false;
        self.reusable = false;
        self
    }
}

/// What an exchange has moved so far.
pub(super) struct Progress {
    /// Request body bytes written to the origin.
    pub(super) received: usize,
    /// Response body bytes written to the client.
    pub(super) sent: usize,
    /// The status that went out.
    pub(super) status: Option<u16>,
}

/// What the request sends to the origin as its `Host`.
enum HostEdit {
    AsSent,
    Set(HeaderValue),
    Remove,
}

pub(super) struct Forward<'a> {
    /// The clock reading at which the request arrived.
    pub(super) started: Instant,
    pub(super) buffer: &'a [u8],
    pub(super) client_ip: Option<std::net::IpAddr>,
    pub(super) head: &'a RequestHead,
    pub(super) request: &'a Request,
    pub(super) route: &'a Arc<HttpRoutePlan>,
    pub(super) id: &'a HeaderValue,
    /// What the route's request header rules do to this request.
    pub(super) values: &'a rules::Evaluated,
    /// Whether the connection serves another request after this one, as far as the request knows.
    pub(super) keepalive: bool,
}

pub(super) fn client_ip(stream: &Stream) -> Option<std::net::IpAddr> {
    stream.get_socket_digest().and_then(|digest| {
        digest
            .peer_addr()
            .and_then(|address| address.as_inet().map(std::net::SocketAddr::ip))
    })
}

/// The values of the request header rules of the route for a request, or `None` when the classic path has to answer
/// it.
pub(super) fn decide(
    stream: &Stream,
    route: &HttpRoutePlan,
    (buffer, client_ip): (&[u8], Option<std::net::IpAddr>),
    head: &RequestHead,
    authority: &Authority,
) -> Option<rules::Evaluated> {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        unreachable!("only proxied routes reach the proxied lane");
    };
    if !route.edits.computes_request_values() {
        return Some(rules::Evaluated::default());
    }
    let source = rules::Source {
        buffer,
        head,
        authority,
        client_ip,
        https: stream.get_ssl().is_some(),
    };
    rules::evaluate(&route.edits, &proxy.policy, &source, buffers::take_write()).ok()
}

impl Forward<'_> {
    fn policy(&self) -> &ProxyPolicyPlan {
        let HttpActionPlan::Proxy(proxy) = &self.route.action else {
            unreachable!("only proxied routes reach the proxied lane");
        };
        &proxy.policy
    }
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
        let mut ctx = self.begin(&forward, authority);
        let mut progress = Progress {
            received: 0,
            sent: 0,
            status: None,
        };
        let outcome = self
            .exchange(
                stream,
                connection,
                &mut ctx,
                upstream,
                &forward,
                &mut progress,
            )
            .await;
        let error = outcome.err();
        if let Some(error) = &error {
            warn!(
                "HTTP request failed with {:?} from {:?}",
                error.etype(),
                error.esource()
            );
            progress.status = self
                .fail(stream, &mut ctx, error, &forward, &mut progress)
                .await;
        }
        self.finish(&mut ctx, &forward, error.as_deref(), &progress);
        connection.end_request();
        if error.is_none() && connection.will_keepalive() {
            return Served::Next(ctx.now);
        }
        let _ = stream.shutdown().await;
        Served::Close
    }

    /// The request context of a request the lane serves, with the facts the classic request filter establishes.
    fn begin(&self, forward: &Forward<'_>, authority: Authority) -> Box<HttpRequestContext> {
        let (buffer, request, route) = (forward.buffer, forward.request, forward.route);
        let HttpActionPlan::Proxy(proxy) = &route.action else {
            unreachable!("only proxied routes reach the proxied lane");
        };
        // Boxed: the context is 1.9 kB and would be copied into every future that holds or returns it.
        let mut ctx = Box::new(HttpRequestContext::new(
            self.metrics.clone(),
            Some(Arc::clone(&self.generation)),
            forward.started,
            self.service.upstream_io_timeout(),
        ));
        ctx.request_id = Some(forward.id.clone());
        ctx.adopt_client_timeouts([self.timeouts.request, self.timeouts.session_write()]);
        let method = Method::from_bytes(request.method(buffer))
            .expect("the route was selected for this method");
        ctx.register_routed_arrival(
            (
                &method,
                request.host(buffer),
                request.path(buffer),
                forward.client_ip,
            ),
            route.route_id.shared(),
            proxy.pool.label().shared(),
            self.service.access_log(),
        );
        ctx.authority = Some(authority);
        ctx.route = Some(Arc::clone(route));
        let policy = &proxy.policy;
        let replay_safe = policy.request_is_replay_safe(
            &method,
            &http::HeaderMap::new(),
            request.body_len == 0,
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

    /// The origin's answer, sent to the client; the status that went out is left in `progress`.
    async fn exchange(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
        progress: &mut Progress,
    ) -> Result<(), Box<Error>> {
        let opened = self
            .attempts(stream, ctx, upstream, forward, progress)
            .await?;
        progress.status = Some(opened.status);
        if !opened.keepalive {
            connection.disable_keepalive();
        }
        self.relay(stream, ctx, upstream, opened, progress)
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
        progress: &mut Progress,
    ) -> Result<Opened, Box<Error>> {
        loop {
            let cancelled = ctx.arm_evacuation_for_lane();
            let outcome = {
                let attempt = self.attempt(stream, ctx, upstream, forward, progress);
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
        progress: &mut Progress,
    ) -> Result<Opened, Box<Error>> {
        let timeouts = ctx
            .next_attempt(
                |_| None,
                |endpoint, policy, authority| match policy {
                    HttpUpstreamHost::PreserveIncoming => Ok(None),
                    policy => selected_upstream_host(endpoint, policy, authority),
                },
            )
            .await?;
        let (session, reused, release) = match Self::idle_session(ctx, upstream).await {
            Some(idle) => idle,
            None => Self::connect(ctx, upstream, timeouts).await?,
        };
        self.send_and_open(
            stream,
            ctx,
            session,
            (reused, release),
            &timeouts,
            forward,
            progress,
        )
        .await
        .map_err(|error| ctx.attempt_failed_for_lane(error))
    }

    /// An idle connection to the selected server, when its address is fixed: no peer is built to ask for one.
    async fn idle_session(
        ctx: &HttpRequestContext,
        upstream: &Connector,
    ) -> Option<(Box<HttpSession>, bool, ReusePeer)> {
        let peer = ctx.reuse_peer()?;
        let session = upstream.reused_boxed_http_session(&peer).await?;
        Some((session, true, peer))
    }

    /// A session on a new connection, or on an idle one for a server whose address is resolved per attempt.
    async fn connect(
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        timeouts: AttemptTimeouts,
    ) -> Result<(Box<HttpSession>, bool, ReusePeer), Box<Error>> {
        let peer = ctx.prepare_peer(timeouts).await?;
        match upstream.get_http_session(&*peer).await {
            Ok((session, reused)) => Ok((Box::new(session), reused, ReusePeer::of(&peer))),
            Err(mut error) => {
                error.as_up();
                Err(ctx.connect_failed(error).into_up())
            }
        }
    }

    /// Sends the request on a connected session and reads the response head.
    async fn send_and_open(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        mut session: Box<HttpSession>,
        (reused, release): (bool, ReusePeer),
        timeouts: &AttemptTimeouts,
        forward: &Forward<'_>,
        progress: &mut Progress,
    ) -> Result<Opened, Box<Error>> {
        ctx.connected(reused, Some(session.digest()))?;
        timeouts.apply(&mut session);
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
        if forward.request.body_len == 0 {
            return self.open(stream, ctx, session, release, forward).await;
        }
        match send_body(ctx, &mut session, forward, progress).await {
            Ok(()) => self.open(stream, ctx, session, release, forward).await,
            // An origin that answered and closed without reading the body is not a failure of the request: its answer
            // goes to the client, and neither connection is used again.
            Err(error) if error.esource() == &pingora::ErrorSource::Upstream => {
                let opened = self.open(stream, ctx, session, release, forward).await;
                opened.map(Opened::unfinished).map_err(|_| error)
            }
            Err(error) => Err(error),
        }
    }

    /// Reads the response head, applies what the classic response filter applies and renders what the client gets.
    async fn open(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        mut session: Box<HttpSession>,
        release: ReusePeer,
        forward: &Forward<'_>,
    ) -> Result<Opened, Box<Error>> {
        let raw = session.read_raw_head().await.map_err(Error::into_up)?;
        let now = ctx.refresh_clock();
        let keepalive = forward.keepalive && !self.generation.retired();
        let mut head = buffers::take_write();
        let (status, framing, refused, keepalive, buffered) =
            if let Some((status, framing, refused, buffered)) =
                serve_fast(&mut head, &raw, &mut session, forward, keepalive, now)
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
            release,
            head,
            framing,
            status,
            keepalive,
            buffered,
            reusable: true,
        })
    }

    /// Writes the response head and relays the body, then gives the origin connection back to the pool.
    async fn relay(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        opened: Opened,
        progress: &mut Progress,
    ) -> Result<(), Box<Error>> {
        let Opened {
            mut session,
            release,
            head,
            framing,
            buffered,
            reusable,
            ..
        } = opened;
        let deadline = tokio::time::Instant::from_std(ctx.deadline);
        let body = async {
            // `frame` carries the head and then each piece of the body with its chunk framing. `progress.sent` counts
            // the body, as a session does.
            let mut frame = head;
            if !buffered {
                write(stream, &frame).await?;
                flush(stream).await?;
                frame.clear();
            }
            let mut first = true;
            while let Some(data) = session.read_body_bytes().await.map_err(Error::into_up)? {
                progress.sent += data.len();
                // The first piece follows the clock reading of the head.
                let now = if std::mem::take(&mut first) {
                    ctx.now
                } else {
                    Instant::now()
                };
                ctx.observe_bytes(progress.received, progress.sent, now);
                let chunked = framing == Framing::Chunked;
                if chunked {
                    write!(frame, "{:x}\r\n", data.len()).expect("a buffer grows");
                }
                if data.len() < DIRECT_BODY {
                    frame.extend_from_slice(&data);
                    if chunked {
                        frame.extend_from_slice(b"\r\n");
                    }
                    write(stream, &frame).await?;
                } else {
                    // The staged head leaves with the piece in one vectored write, the piece is not copied.
                    write(stream, &frame).await?;
                    write(stream, &data).await?;
                    if chunked {
                        write(stream, b"\r\n").await?;
                    }
                }
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
        if reusable {
            upstream
                .release_boxed_http_session(session, &release, release.idle_timeout())
                .await;
        }
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
        progress: &mut Progress,
    ) -> Option<u16> {
        let sent_status = progress.status;
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
                    progress.sent += body;
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
        forward: &Forward<'_>,
        error: Option<&Error>,
        progress: &Progress,
    ) {
        let (status, sent, received) = (progress.status, progress.sent, progress.received);
        let now = ctx.refresh_clock();
        let elapsed = now.saturating_duration_since(ctx.started_at);
        let result = classify_http_result(error, status);
        ctx.operation_result = Some(result);
        if let Err(metric_error) = self.metrics.record_http_operation(result, elapsed) {
            warn!("could not account for HTTP operation metrics: {metric_error}");
        }
        ctx.observe_bytes(received, sent, now);
        ctx.release_lease();
        ctx.account_attempt(received, sent, now);
        ctx.record_failed_evacuations(error);
        if let Some(log) = self.service.access_log() {
            super::log::write(log, ctx, forward, progress, elapsed);
        }
    }
}

/// Sends the request body, which arrived with the head, to the origin.
async fn send_body(
    ctx: &mut HttpRequestContext,
    session: &mut HttpSession,
    forward: &Forward<'_>,
    progress: &mut Progress,
) -> Result<(), Box<Error>> {
    let body = forward.request.body(forward.buffer);
    session.init_request_body(FastFraming {
        chunked: false,
        content_length: Some(body.len()),
    });
    session.write_body(body).await.map_err(Error::into_up)?;
    progress.received = body.len();
    ctx.observe_bytes(progress.received, 0, Instant::now());
    session.finish_body().await.map_err(Error::into_up)?;
    Ok(())
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
    // The host a rule may name: the request's own when it is passed on as it is.
    let incoming;
    let selected = match host {
        HostEdit::Set(value) => Some(value),
        HostEdit::AsSent if forward.values.names_host() => {
            incoming = HeaderValue::from_bytes(head.host(buffer)).ok();
            incoming.as_ref()
        }
        HostEdit::AsSent | HostEdit::Remove => None,
    };
    let policy = forward.policy();
    edit(rules::edit_request(
        &mut edits,
        &forward.route.edits,
        policy,
        forward.values,
        selected,
    ));
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
    now: Instant,
) -> Option<(u16, Framing, bool, bool)> {
    let mut head = buffers::take_response_head();
    let served =
        (scan_response(raw, &mut head) == ScanResponse::Head && is_plain(&head)).then(|| {
            let framing = render_fast(out, raw, &head, session, forward, keepalive, now);
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
    now: Instant,
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
    edit(rules::edit_response(
        &mut edits,
        &forward.route.edits,
        forward.policy(),
        head.status,
    ));
    let no_body = is_head || matches!(head.status, 204 | 304);
    let chunked = !no_body && facts.content_length.is_none();
    edit(edits.stamp_request_id(raw, head, Value::trusted(forward.id.as_bytes())));
    if chunked {
        edit(edits.set(names::TRANSFER_ENCODING, Value::trusted(b"chunked")));
    }
    let date = get_cached_date_at(Duration::from_millis(wall_ms(now)));
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
