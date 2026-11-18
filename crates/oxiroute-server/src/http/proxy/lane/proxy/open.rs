use std::time::Instant;

use bytes::{Bytes, BytesMut};
use futures_util::FutureExt as _;
use http::StatusCode;
use pingora::{
    Error, ErrorType,
    protocols::{
        Stream,
        http::v1::{client::HttpSession, server::render_final_response_head},
    },
};

use super::{
    Forward, Opened, Progress,
    wire::{prepare_slow, read_final_head, read_head, send_arrived},
};
use crate::{
    HealthFailure,
    http::{
        inflight::Phase,
        proxy::{
            HttpRequestContext,
            attempt::AttemptTimeouts,
            lane::{
                buffers,
                render::{host_edit, push_small_body, render_request},
                response::serve_fast,
                upload::Upload,
            },
            proxy_policy,
            retry::status_retry_possible,
        },
    },
    observability::upstream_stats::AttemptOutcome,
    upstream_peer::{POOL_UNAVAILABLE, POOL_UNAVAILABLE_HEADER, ReusePeer},
};

impl super::super::LaneFront {
    /// Sends the request on a connected session and reads the response head.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn send_and_open(
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
            (forward, progress),
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
        (forward, progress): (&Forward<'_>, &mut Progress),
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
                let mut response =
                    read_final_head(stream, ctx, &mut session, (forward, progress)).await?;
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
        ctx.ensure_open_at(now, true)?;
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
}
