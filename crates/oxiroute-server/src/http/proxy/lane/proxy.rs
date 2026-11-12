//! The proxied lane: a request is forwarded to an HTTP/1.1 server of a pool on the connection itself, with its body,
//! which is streamed to the origin as it arrives when it did not come with the head (`upload`). The head goes upstream and the response head comes back through the head codec,
//! without a header map on either side; a response head the codec does not know is read by the classic client parser, so the response is
//! always served. Selection, leases, timeouts, health signals, evacuation and accounting are the classic request
//! context's own steps.

use std::{
    future::{Future as _, poll_fn},
    io::Write as _,
    pin::pin,
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};

use bytes::{BufMut as _, Bytes, BytesMut};
use futures_util::FutureExt as _;
use http::{HeaderValue, Method, StatusCode, Version, uri::Authority};
use log::{debug, warn};
use oxiroute_config::HttpUpstreamHost;
use oxiroute_h1_head::RequestHead;
use pingora::{
    Error, ErrorType,
    apps::Http1Connection,
    connectors::http::v1::Connector,
    protocols::{
        Stream,
        http::v1::{
            client::{FastFraming, HttpSession},
            server::{render_final_response_head, render_informational_head},
        },
    },
    upstreams::peer::Peer as _,
};
use tokio::io::AsyncWriteExt as _;

use super::{
    Served, buffers,
    head::Request,
    render::{host_edit, push_small_body, render_request},
    response::serve_fast,
    rules,
    timers::{Wait, within, within_for as within_step},
    timing::Expired,
    upload::Upload,
};
use crate::{
    HealthFailure,
    http::{
        action::{HttpActionPlan, HttpRoutePlan, ProxyActionPlan, ProxyPolicyPlan},
        evacuation::LeftRunning,
        inflight::Phase,
        proxy::{
            ErrorReason, HttpRequestContext, apply_response_policy,
            attempt::{AttemptTimeouts, Exchanged},
            classify_http_result, error_reason, error_response, failure_reason, proxy_error_status,
            proxy_policy,
            retry::status_retry_possible,
            selected_upstream_host,
        },
        request_id::stamp_response,
    },
    observability::upstream_stats::AttemptOutcome,
    routing::FirstHeader,
    upstream_peer::{POOL_UNAVAILABLE, POOL_UNAVAILABLE_HEADER, ReusePeer},
};

/// The log target of the debug line that says a request was served by the lane, with its id: how a test (or an
/// operator who raises the level of this target) tells the lane from the classic path.
pub(crate) const LOG_TARGET: &str = "oxiroute::lane";

/// A body piece of at least this size is written as it was read; smaller ones are copied behind the head.
const DIRECT_BODY: usize = 16 * 1024;

/// How the body of the response goes to the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Framing {
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
    /// What is left of the request body, which goes on while the response is relayed.
    upload: Upload,
    /// What the attempt allows each step of the response.
    timeouts: AttemptTimeouts,
}

/// What an exchange has moved so far.
pub(in crate::http::proxy) struct Progress {
    /// Request body bytes written to the origin.
    pub(in crate::http::proxy) received: usize,
    /// Response body bytes written to the client.
    pub(in crate::http::proxy) sent: usize,
    /// The status that went out.
    pub(in crate::http::proxy) status: Option<u16>,
}

pub(super) struct Forward<'a> {
    /// The clock reading at which the request arrived.
    pub(super) started: Instant,
    pub(super) buffer: &'a [u8],
    pub(super) method: &'a Method,
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
pub(in crate::http::proxy) fn decide(
    https: bool,
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
        https,
    };
    rules::evaluate(&route.edits, &proxy.policy, &source, buffers::take_write()).ok()
}

impl Forward<'_> {
    pub(super) fn policy(&self) -> &ProxyPolicyPlan {
        &proxy_route(self.route).policy
    }
}

/// The proxy action of a route the lane serves as a proxy.
pub(in crate::http::proxy) fn proxy_route(route: &HttpRoutePlan) -> &ProxyActionPlan {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        unreachable!("only proxied routes reach the proxied lane");
    };
    proxy
}

/// What a request context needs of the request that arrived, whatever protocol it arrived by.
pub(in crate::http::proxy) struct Arrival<'a> {
    pub(in crate::http::proxy) started: Instant,
    /// The route, the id of the request and its authority.
    pub(in crate::http::proxy) route: (&'a Arc<HttpRoutePlan>, &'a HeaderValue, Authority),
    /// The method, the host, the path and the client's address.
    pub(in crate::http::proxy) line: (
        &'a Method,
        Option<&'a str>,
        &'a str,
        Option<std::net::IpAddr>,
    ),
    /// The fields of the request, as a queue tag reads them.
    pub(in crate::http::proxy) fields: Scanned<'a>,
    /// Whether the request could be sent again: an operator's move of it needs that.
    pub(in crate::http::proxy) replay_safe: bool,
}

impl super::LaneFront {
    pub(super) async fn serve_proxied(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        forward: Forward<'_>,
        authority: Authority,
    ) -> Served {
        debug!(
            target: LOG_TARGET,
            "served by the HTTP/1 lane: request_id={}",
            forward.id.to_str().unwrap_or_default()
        );
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
        super::end_connection(stream).await;
        Served::Close
    }

    /// The request context of a request the lane serves, with the facts the classic request filter establishes.
    fn begin(&self, forward: &Forward<'_>, authority: Authority) -> Box<HttpRequestContext> {
        let (buffer, request, route) = (forward.buffer, forward.request, forward.route);
        let replay_safe = proxy_route(route).policy.request_is_replay_safe_with(
            forward.method,
            |name| {
                let name = name.as_str().as_bytes();
                let key = forward
                    .head
                    .headers()
                    .iter()
                    .find(|header| header.name(buffer).eq_ignore_ascii_case(name));
                key.is_some_and(|header| !header.value(buffer).is_empty())
            },
            request.body_len == 0,
            route.policy.request_buffering,
        );
        self.begin_arrival(Arrival {
            started: forward.started,
            route: (route, forward.id, authority),
            line: (
                forward.method,
                request.host(buffer),
                request.path(buffer),
                forward.client_ip,
            ),
            fields: Scanned(buffer, forward.head),
            replay_safe,
        })
    }

    /// The request context of a request the lane serves, whatever protocol it arrived by.
    pub(in crate::http::proxy) fn begin_arrival(
        &self,
        arrival: Arrival<'_>,
    ) -> Box<HttpRequestContext> {
        let Arrival {
            started,
            route: (route, id, authority),
            line,
            fields,
            replay_safe,
        } = arrival;
        let proxy = proxy_route(route);
        // Boxed: the context is 1.9 kB and would be copied into every future that holds or returns it.
        let mut ctx = Box::new(HttpRequestContext::new(
            self.metrics.clone(),
            Some(Arc::clone(&self.generation)),
            started,
            self.service.upstream_io_timeout(),
        ));
        ctx.request_id = Some(id.clone());
        ctx.adopt_client_timeouts([self.timeouts.request, self.timeouts.session_write()]);
        ctx.register_routed_arrival(
            line,
            route.route_id.shared(),
            proxy.pool.label().shared(),
            self.service.access_log(),
        );
        ctx.authority = Some(authority);
        ctx.route = Some(Arc::clone(route));
        let policy = &proxy.policy;
        ctx.connection_retryable = policy.max_retries > 0;
        ctx.replay_retryable = policy.max_retries > 0 && replay_safe;
        ctx.evacuation_pin = LeftRunning::unmovable(false, replay_safe);
        if let Some(budget) = &policy.retry_budget {
            budget.record_request(ctx.now);
        }
        if proxy.pool.selector().schedules_queue() {
            ctx.queue_tag = proxy
                .pool
                .selector()
                .queue_tag(policy.queue_class.as_deref(), &fields);
        }
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
            .map_err(|error| {
                let exchanged = Exchanged {
                    body_replayable: progress.received == 0,
                    response_retryable: false,
                };
                ctx.attempt_failed(error, exchanged)
            })
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
            let mut cancelled = pin!(ctx.arm_evacuation_for_lane());
            // Polled in place: `select!` would move the attempt's future, which is kilobytes, into its own tuple.
            let raced = {
                let mut attempt = pin!(self.attempt(stream, ctx, upstream, forward, progress));
                poll_fn(|cx| match cancelled.as_mut().poll(cx) {
                    Poll::Ready(error) => Poll::Ready(Err(error)),
                    Poll::Pending => attempt.as_mut().poll(cx).map(Ok),
                })
                .await
            };
            let outcome = match raced {
                Ok(opened) => opened,
                Err(error) => Err(ctx.attempt_failed(error, Exchanged::UNANSWERED)),
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
        .map_err(|error| {
            let exchanged = Exchanged {
                body_replayable: progress.received == 0,
                response_retryable: true,
            };
            ctx.attempt_failed(error, exchanged)
        })
    }

    /// An idle connection to the selected server, when its address is fixed: no peer is built to ask for one.
    pub(in crate::http::proxy) async fn idle_session(
        ctx: &HttpRequestContext,
        upstream: &Connector,
    ) -> Option<(Box<HttpSession>, bool, ReusePeer)> {
        let peer = ctx.reuse_peer()?;
        let session = upstream.reused_boxed_http_session(&peer).await?;
        Some((session, true, peer))
    }

    /// A session on a new connection, or on an idle one for a server whose address is resolved per attempt.
    pub(in crate::http::proxy) async fn connect(
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
    #[allow(clippy::too_many_arguments)]
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
        timeouts.apply_write(&mut session);
        ctx.remaining_cached(true)?;
        let host = host_edit(ctx);
        let connection_close = ctx.pool.as_ref().is_some_and(|pool| {
            pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
        });
        let mut request = buffers::take_write();
        render_request(&mut request, forward, &host, connection_close);
        let with_head = push_small_body(&mut request, forward);
        let written = session
            .write_request_head_raw(&request, forward.request.is_head)
            .await;
        buffers::give_write(request);
        written.map_err(Error::into_up)?;
        let mut upload = Upload::new(
            forward.request.body_len,
            forward.buffer.len() - forward.request.head_len,
        );
        let raw = if forward.request.body_len == 0 {
            read_head(ctx, &mut session, timeouts).await?
        } else {
            self.head_of(
                stream,
                ctx,
                &mut session,
                (forward, with_head),
                (&mut upload, progress),
                timeouts,
            )
            .await?
        };
        self.open(
            stream,
            ctx,
            (session, release),
            forward,
            (raw, upload),
            *timeouts,
        )
        .await
    }

    /// The origin's response head. The body goes to the origin as the client sends it, and an answer that comes before
    /// the body ends is the response: the rest of the upload is not sent. An origin that answered and then did not
    /// take the body still has its answer relayed.
    async fn head_of(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        session: &mut HttpSession,
        request: (&Forward<'_>, bool),
        (upload, progress): (&mut Upload, &mut Progress),
        timeouts: &AttemptTimeouts,
    ) -> Result<Bytes, Box<Error>> {
        match self
            .upload_until_head(stream, ctx, session, request, (upload, progress), timeouts)
            .await
        {
            Err(error) if error.esource() == &pingora::ErrorSource::Upstream => {
                let raw = read_head(ctx, session, timeouts).await.map_err(|_| error)?;
                upload.refuse();
                Ok(raw)
            }
            outcome => outcome,
        }
    }

    async fn upload_until_head(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        session: &mut HttpSession,
        (forward, with_head): (&Forward<'_>, bool),
        (upload, progress): (&mut Upload, &mut Progress),
        timeouts: &AttemptTimeouts,
    ) -> Result<Bytes, Box<Error>> {
        send_arrived(ctx, session, (forward, with_head), progress).await?;
        while upload.pumping() {
            // What the client already sent goes first, without arming the origin's read timer for each piece.
            let ready = if let Some(ready) = upload.readable(stream, None).now_or_never() {
                ready
            } else {
                tokio::select! {
                    biased;
                    raw = read_head(ctx, session, timeouts) => return raw,
                    ready = upload.readable(stream, self.timeouts.request) => ready,
                }
            };
            ready?;
            progress.received += upload.forward(stream, session).await?;
            ctx.observe_bytes(progress.received, 0, Instant::now());
        }
        read_head(ctx, session, timeouts).await
    }

    /// Reads the response head, applies what the classic response filter applies and renders what the client gets.
    async fn open(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        (mut session, release): (Box<HttpSession>, ReusePeer),
        forward: &Forward<'_>,
        (raw, upload): (Bytes, Upload),
        timeouts: AttemptTimeouts,
    ) -> Result<Opened, Box<Error>> {
        let now = ctx.refresh_clock();
        // The client connection goes on only when the whole body arrived: what is left of it cannot be told from the
        // next request.
        let keepalive = forward.keepalive && !self.generation.retired() && upload.is_complete();
        let mut head = buffers::take_write();
        let queue = ctx.queue_stamp();
        let (status, framing, refused, keepalive, buffered) =
            if let Some((status, framing, refused, buffered)) = serve_fast(
                &mut head,
                &raw,
                &mut session,
                forward,
                keepalive,
                (now, queue),
            ) {
                (status, framing, refused, keepalive, buffered)
            } else {
                timeouts.apply_reads(&mut session);
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
        let retries_on_status = refused || proxy_policy(ctx).retries_on_status(status);
        if retries_on_status
            && status_retry_possible(ctx, refused, false)
            && ctx.spend_retry_budget()
        {
            // The origin keeps what it has not sent: its connection is not used again.
            ctx.retry_prepaid = true;
            ctx.trace.finish(AttemptOutcome::StatusRetry, Some(status));
            let mut error = if refused {
                Error::new_up(POOL_UNAVAILABLE)
            } else {
                Error::new_up(ErrorType::HTTPStatus(status))
            };
            error.set_retry(true);
            return Err(error);
        }
        ctx.commit_response(StatusCode::from_u16(status).expect("a scanned status"))?;
        if retries_on_status {
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
            upload,
            timeouts,
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
            mut upload,
            timeouts,
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
            while let Some(data) = self
                .next_piece(
                    stream,
                    ctx,
                    &mut session,
                    (&mut upload, progress),
                    &timeouts,
                )
                .await?
            {
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
        within(Wait::Total, deadline, body)
            .await
            .map_err(|Expired| Error::new_up(ErrorType::ReadTimedout))??;
        // An origin that did not get the whole body keeps what it has not read: its connection is not used again.
        if upload.is_complete() {
            upstream
                .release_boxed_http_session(session, &release, release.idle_timeout())
                .await;
        }
        Ok(())
    }

    /// The next piece of the origin's response body. What is left of the request body goes to the origin meanwhile,
    /// as it arrives.
    async fn next_piece(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        session: &mut HttpSession,
        (upload, progress): (&mut Upload, &mut Progress),
        timeouts: &AttemptTimeouts,
    ) -> Result<Option<Bytes>, Box<Error>> {
        while upload.pumping() {
            tokio::select! {
                piece = read_piece(session, timeouts) => return piece,
                ready = upload.readable(stream, self.timeouts.request) => {
                    ready?;
                    match upload.forward(stream, session).await {
                        Ok(count) => {
                            progress.received += count;
                            ctx.observe_bytes(progress.received, progress.sent, Instant::now());
                        }
                        // The origin took no more of the body; its response goes on.
                        Err(error) if error.esource() == &pingora::ErrorSource::Upstream => upload.refuse(),
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        read_piece(session, timeouts).await
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
        let (code, reason) = match error_answer(ctx, error, progress.status) {
            Ok(answer) => answer,
            Err(sent) => return sent,
        };
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
        let elapsed = self.finish_request(ctx, error, progress);
        if let Some(log) = self.service.access_log() {
            super::log::write_proxied(log, &self.service_log, ctx, forward, progress, elapsed);
        }
    }

    /// The accounting of a request that ended, whatever protocol it arrived by; the time it took.
    pub(in crate::http::proxy) fn finish_request(
        &self,
        ctx: &mut HttpRequestContext,
        error: Option<&Error>,
        progress: &Progress,
    ) -> Duration {
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
        elapsed
    }
}

/// The error response a request that ended in `error` gets, the status and the reason it names. `Err` when none is
/// sent: the status of the response that went out already, if there was one.
pub(in crate::http::proxy) fn error_answer(
    ctx: &mut HttpRequestContext,
    error: &Error,
    sent_status: Option<u16>,
) -> Result<(u16, Option<ErrorReason>), Option<u16>> {
    let started = sent_status.is_some();
    ctx.trace.failure_reason = Some(failure_reason(error, ctx, started));
    if started || ctx.client_deadline_expired() {
        return Err(sent_status);
    }
    let reason = error_reason(error, ctx, false);
    let code = reason.map_or_else(|| proxy_error_status(error), ErrorReason::status);
    if let Some(reason) = reason {
        debug!("request ended by {}", reason.as_str());
    }
    if code == 0 {
        return Err(None);
    }
    Ok((code, reason))
}

/// Sends the part of the request body that arrived with the head to the origin; all of it ends the body. Nothing
/// is written for a body that went with the head: the session's body is complete with it.
async fn send_arrived(
    ctx: &mut HttpRequestContext,
    session: &mut HttpSession,
    (forward, with_head): (&Forward<'_>, bool),
    progress: &mut Progress,
) -> Result<(), Box<Error>> {
    let body = forward.request.body(forward.buffer);
    if !with_head {
        session.init_request_body(FastFraming {
            chunked: false,
            content_length: Some(forward.request.body_len),
        });
        if !body.is_empty() {
            session.write_body(body).await.map_err(Error::into_up)?;
        }
        if body.len() == forward.request.body_len {
            session.finish_body().await.map_err(Error::into_up)?;
        }
    }
    if !body.is_empty() {
        progress.received = body.len();
        ctx.observe_bytes(progress.received, 0, Instant::now());
    }
    Ok(())
}

/// The origin's response head, within the time the attempt allows for it. The lane keeps this limit itself: a
/// session's own read limit is a timer from the wheel of its clock thread for every request.
async fn read_head(
    ctx: &HttpRequestContext,
    session: &mut HttpSession,
    timeouts: &AttemptTimeouts,
) -> Result<Bytes, Box<Error>> {
    let deadline = tokio::time::Instant::from_std(ctx.now + timeouts.read());
    match within(Wait::Step, deadline, session.read_raw_head()).await {
        Ok(head) => head.map_err(Error::into_up),
        Err(Expired) => {
            Err(Error::explain(ErrorType::ReadTimedout, "while reading response headers").into_up())
        }
    }
}

/// The next piece of the origin's response body, within the idle time the attempt allows between pieces.
async fn read_piece(
    session: &mut HttpSession,
    timeouts: &AttemptTimeouts,
) -> Result<Option<Bytes>, Box<Error>> {
    match within_step(
        Wait::Step,
        Some(timeouts.body_read()),
        session.read_body_bytes(),
    )
    .await
    {
        Ok(piece) => piece.map_err(Error::into_up),
        Err(Expired) => Err(Error::explain(ErrorType::ReadTimedout, "reading body").into_up()),
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
    ctx.stamp_queue_headers(response)?;
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

/// The scanned head of a request with the bytes it was scanned from, for what a queue tag reads of it.
pub(in crate::http::proxy) struct Scanned<'a>(
    pub(in crate::http::proxy) &'a [u8],
    pub(in crate::http::proxy) &'a RequestHead,
);

impl FirstHeader for Scanned<'_> {
    fn first(&self, name: &http::HeaderName) -> Option<&str> {
        let (buffer, name) = (self.0, name.as_str().as_bytes());
        let header = self
            .1
            .headers()
            .iter()
            .find(|header| header.name(buffer).eq_ignore_ascii_case(name))?;
        // What `HeaderValue::to_str` accepts: visible ASCII and tabs.
        let value = header.value(buffer);
        value
            .iter()
            .all(|byte| matches!(byte, b' '..=b'~' | b'\t'))
            .then(|| std::str::from_utf8(value).expect("visible ASCII is UTF-8"))
    }
}
