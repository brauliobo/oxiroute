//! The proxied exchange: the classic request context's own steps (selection, lease, queue, deadlines, passive
//! health, evacuation, accounting, error mapping) driven without a session, as the HTTP/1 lane drives them. The
//! request goes to a plain HTTP/1.1 server of the pool and its answer comes back on the stream; the client leaving
//! ends the exchange and the origin connection with it.

use std::time::Instant;

use bytes::Bytes;
use http::StatusCode;
use log::warn;
use oxiroute_h1_head::{ScanResponse, scan_response};
use pingora::{
    Error, ErrorType,
    connectors::http::v1::Connector,
    http::ResponseHeader,
    protocols::http::{v1::client::HttpSession, v2::server::H2Stream},
    upstreams::peer::Peer as _,
};

use super::{
    claim::Plan,
    fast, head, relay,
    response::{self, Head},
};
use crate::{
    HealthFailure,
    http::{
        inflight::Phase,
        proxy::{
            HttpRequestContext, apply_response_policy,
            attempt::{AttemptTimeouts, Exchanged},
            error_response,
            lane::{
                LaneFront, buffers,
                proxy::{Arrival, Progress, Scanned, error_answer, proxy_route},
                response::{QueueValues, accept_head, is_plain, refused_by_pool},
                rules,
                timing::{Expired, within},
            },
            proxy_policy,
            retry::status_retry_possible,
            selected_upstream_host,
        },
        request_id::stamp_response,
    },
    observability::upstream_stats::AttemptOutcome,
    upstream_peer::{POOL_UNAVAILABLE, POOL_UNAVAILABLE_HEADER, ReusePeer},
};

/// An origin response whose head is ready to go to the client.
struct Opened {
    session: Box<HttpSession>,
    /// Where the connection goes back to when the response is served.
    release: ReusePeer,
    head: Head,
    /// The head ends the response: no body follows.
    end: bool,
}

pub(super) async fn serve(front: &LaneFront, mut stream: H2Stream, plan: Plan) {
    let mut ctx = begin(front, &plan);
    let mut progress = Progress {
        received: 0,
        sent: 0,
        status: None,
    };
    let outcome = exchange(front, &mut stream, &mut ctx, &plan, &mut progress).await;
    let error = outcome.err();
    if let Some(error) = &error {
        warn!(
            "HTTP request failed with {:?} from {:?}",
            error.etype(),
            error.esource()
        );
        progress.status = fail(front, &mut stream, &mut ctx, error, &plan, &mut progress).await;
    }
    front.finish_request(&mut ctx, error.as_deref(), &progress);
    plan.recycle();
}

fn begin(front: &LaneFront, plan: &Plan) -> Box<HttpRequestContext> {
    let wire = plan.wire.as_ref().expect("a proxied request has its wire");
    front.begin_arrival(Arrival {
        started: Instant::now(),
        route: (&plan.route, &plan.id, plan.authority.clone()),
        line: (
            &plan.method,
            None,
            wire.request.path(&wire.text),
            plan.client_ip,
        ),
        fields: Scanned(&wire.text, &wire.head),
        replay_safe: plan.replay_safe,
    })
}

/// The origin's answer, sent to the client; the status that went out is left in `progress`.
async fn exchange(
    front: &LaneFront,
    stream: &mut H2Stream,
    ctx: &mut HttpRequestContext,
    plan: &Plan,
    progress: &mut Progress,
) -> Result<(), Box<Error>> {
    let upstream = &*front.upstream;
    let opened = tokio::select! {
        biased;
        opened = attempts(ctx, upstream, plan) => opened?,
        left = relay::client_left(stream) => return Err(ctx.attempt_failed(left, Exchanged::UNANSWERED)),
    };
    progress.status = Some(opened.head.status().as_u16());
    let answered = Exchanged {
        body_replayable: true,
        response_retryable: false,
    };
    relay_response(front, stream, ctx, upstream, opened, progress)
        .await
        .map_err(|error| ctx.attempt_failed(error, answered))
}

/// Attempts on the pool's servers until one answers or the request fails; an attempt an operator moved, or one the
/// route's retry policy allows, goes on to another server.
async fn attempts(
    ctx: &mut HttpRequestContext,
    upstream: &Connector,
    plan: &Plan,
) -> Result<Opened, Box<Error>> {
    loop {
        let cancelled = ctx.arm_evacuation_for_lane();
        let outcome = {
            let attempt = attempt(ctx, upstream, plan);
            tokio::select! {
                biased;
                error = cancelled => Err(ctx.attempt_failed(error, Exchanged::UNANSWERED)),
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
    ctx: &mut HttpRequestContext,
    upstream: &Connector,
    plan: &Plan,
) -> Result<Opened, Box<Error>> {
    let timeouts = ctx.next_attempt(|_| None, selected_upstream_host).await?;
    let (session, reused, release) = match LaneFront::idle_session(ctx, upstream).await {
        Some(idle) => idle,
        None => LaneFront::connect(ctx, upstream, timeouts).await?,
    };
    send_and_open(ctx, session, (reused, release), &timeouts, plan)
        .await
        .map_err(|error| ctx.attempt_failed(error, Exchanged::UNANSWERED))
}

/// Sends the request on a connected session and reads the response head.
async fn send_and_open(
    ctx: &mut HttpRequestContext,
    mut session: Box<HttpSession>,
    (reused, release): (bool, ReusePeer),
    timeouts: &AttemptTimeouts,
    plan: &Plan,
) -> Result<Opened, Box<Error>> {
    ctx.connected(reused, Some(session.digest()))?;
    timeouts.apply(&mut session);
    ctx.remaining_cached(true)?;
    let host = ctx.selected_upstream_host.take();
    if let Some(host) = &host {
        head::check_host(host, &plan.authority)?;
    }
    let mut request = buffers::take_write();
    render_request(&mut request, ctx, plan, host.as_ref());
    let written = session
        .write_request_head_raw(&request, plan.is_head())
        .await;
    buffers::give_write(request);
    written.map_err(Error::into_up)?;
    open(ctx, session, release, plan).await
}

/// The request head the origin gets: the classic sequence of the edits and then the route's header rules.
fn render_request(
    out: &mut Vec<u8>,
    ctx: &HttpRequestContext,
    plan: &Plan,
    host: Option<&http::HeaderValue>,
) {
    let wire = plan.wire.as_ref().expect("a proxied request has its wire");
    let connection_close = ctx.pool.as_ref().is_some_and(|pool| {
        pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
    });
    let mut edits = head::edits(wire, &plan.id, host, connection_close);
    rules::edit_request(
        &mut edits,
        &plan.route.edits,
        &proxy_route(&plan.route).policy,
        &plan.values,
        host,
    )
    .expect("a request head needs few edits");
    head::render(out, wire, &edits);
}

/// Reads the response head and applies what the classic response filter applies. An informational head is not
/// forwarded on HTTP/2, as the classic session ignores it.
async fn open(
    ctx: &mut HttpRequestContext,
    mut session: Box<HttpSession>,
    release: ReusePeer,
    plan: &Plan,
) -> Result<Opened, Box<Error>> {
    let raw = session.read_raw_head().await.map_err(Error::into_up)?;
    let now = ctx.refresh_clock();
    let queue = ctx.queue_stamp().map(QueueValues::new);
    let (head, refused) = if let Some(fast) = scanned(&raw, &mut session, plan, queue.as_ref()) {
        fast
    } else {
        session.reparse_raw_head();
        classic(ctx, &mut session, plan).await?
    };
    let status = head.status();
    let end = plan.is_head() || matches!(status.as_u16(), 204 | 304) || session.is_body_done();
    ctx.remaining_at(now, true)?;
    let retries_on_status = refused || proxy_policy(ctx).retries_on_status(status.as_u16());
    if retries_on_status && status_retry_possible(ctx, refused, false) && ctx.spend_retry_budget() {
        // The origin keeps what it has not sent: its connection is not used again.
        ctx.retry_prepaid = true;
        ctx.trace
            .finish(AttemptOutcome::StatusRetry, Some(status.as_u16()));
        let mut error = if refused {
            Error::new_up(POOL_UNAVAILABLE)
        } else {
            Error::new_up(ErrorType::HTTPStatus(status.as_u16()))
        };
        error.set_retry(true);
        return Err(error);
    }
    ctx.commit_response(status)?;
    if retries_on_status {
        ctx.record_passive_failure(HealthFailure::UnexpectedStatus);
    } else {
        ctx.record_passive_success();
    }
    ctx.note_response(status.as_u16(), now);
    ctx.enter_phase(Phase::Streaming, now);
    Ok(Opened {
        session,
        release,
        head,
        end,
    })
}

/// The response head for a head the codec scans and knows, and whether the origin refused the request as a pool
/// without servers. `None` leaves the head to the classic parser.
fn scanned(
    raw: &Bytes,
    session: &mut HttpSession,
    plan: &Plan,
    queue: Option<&QueueValues>,
) -> Option<(Head, bool)> {
    let mut head = buffers::take_response_head();
    let served =
        (scan_response(raw, &mut head) == ScanResponse::Head && is_plain(&head)).then(|| {
            accept_head(session, &head);
            let policy = &proxy_route(&plan.route).policy;
            let fields = fast::fields(raw, &head, (&plan.route.edits, policy), queue, &plan.id);
            (
                Head::Fields(fast::status(&head), fields),
                refused_by_pool(raw, &head),
            )
        });
    buffers::give_response_head(head);
    served
}

/// The response head of the classic parser with the classic filter applied.
async fn classic(
    ctx: &mut HttpRequestContext,
    session: &mut HttpSession,
    plan: &Plan,
) -> Result<(Head, bool), Box<Error>> {
    let mut head = final_head(session).await?;
    let refused = head.status == StatusCode::SERVICE_UNAVAILABLE
        && head.headers.contains_key(&POOL_UNAVAILABLE_HEADER);
    apply_response_policy(&mut head, proxy_policy(ctx), true)?;
    ctx.stamp_queue_headers(&mut head)?;
    stamp_response(&mut head, &plan.id)?;
    Ok((Head::Classic(head), refused))
}

async fn final_head(session: &mut HttpSession) -> Result<ResponseHeader, Box<Error>> {
    loop {
        let head = *session
            .read_resp_header_parts()
            .await
            .map_err(Error::into_up)?;
        if head.status == StatusCode::SWITCHING_PROTOCOLS {
            return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
        }
        if !head.status.is_informational() {
            return Ok(head);
        }
    }
}

/// Writes the response head and relays the body, then gives the origin connection back to the pool.
async fn relay_response(
    front: &LaneFront,
    stream: &mut H2Stream,
    ctx: &mut HttpRequestContext,
    upstream: &Connector,
    opened: Opened,
    progress: &mut Progress,
) -> Result<(), Box<Error>> {
    let Opened {
        mut session,
        release,
        head,
        end,
    } = opened;
    let deadline = tokio::time::Instant::from_std(ctx.deadline);
    let write_timeout = front.timeouts.session_write();
    let body = async {
        head.send(stream, end)?;
        let mut first = true;
        // `done`: the origin's body is over; `closed`: `END_STREAM` is on the wire.
        let mut done = end;
        let mut closed = end;
        while !done {
            let piece = tokio::select! {
                biased;
                piece = session.read_body_bytes() => piece.map_err(Error::into_up)?,
                left = relay::client_left(stream) => return Err(left),
            };
            let Some(data) = piece else { break };
            progress.sent += data.len();
            // The first piece follows the clock reading of the head.
            let now = if std::mem::take(&mut first) {
                ctx.now
            } else {
                Instant::now()
            };
            ctx.observe_bytes(0, progress.sent, now);
            done = session.is_body_done();
            if !data.is_empty() {
                relay::send_body(stream, data, done, write_timeout).await?;
                closed = done;
            }
        }
        if !closed {
            relay::finish(stream)?;
        }
        Ok(())
    };
    within(deadline, body)
        .await
        .map_err(|Expired| Error::new_up(ErrorType::ReadTimedout))??;
    upstream
        .release_boxed_http_session(session, &release, release.idle_timeout())
        .await;
    Ok(())
}

/// Answers a request that ended in `error`, unless a response went out already; the status that the client got,
/// if any.
async fn fail(
    front: &LaneFront,
    stream: &mut H2Stream,
    ctx: &mut HttpRequestContext,
    error: &Error,
    plan: &Plan,
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
        Some(&plan.id),
        plan.is_head(),
    );
    let written = match rendered {
        Ok((head, body)) => {
            let length = body.len();
            response::send_local(stream, head, body, front.timeouts.session_write())
                .await
                .map(|()| length)
        }
        Err(error) => Err(error),
    };
    match written {
        Ok(length) => {
            progress.sent += length;
            Some(code)
        }
        Err(write_error) => {
            warn!("failed to send error response downstream: {write_error}");
            None
        }
    }
}
