use std::time::Instant;

use bytes::{BufMut as _, BytesMut};
use http::HeaderValue;
use log::{debug, warn};
use pingora::{
    Error,
    protocols::{Stream, http::v1::server::render_final_response_head},
};

use super::{Forward, Progress, wire::write};
use crate::http::proxy::{
    ErrorReason, HttpRequestContext, error_reason, error_response, failure_reason,
    lane::log::{HTTP1, Proxied},
    proxy_error_status,
};

impl super::super::LaneFront {
    /// Answers a request that ended in `error`, unless a response went out already; the status of the response
    /// that the client got, if any.
    pub(super) async fn fail(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        error: &Error,
        forward: &Forward<'_>,
        progress: &mut Progress,
    ) -> Option<u16> {
        let (code, response, body) = match failure_response(
            ctx,
            error,
            forward.id,
            forward.request.is_head,
            progress.status,
        ) {
            Ok(rendered) => rendered,
            Err(sent) => return sent,
        };
        match write(stream, &response).await {
            Ok(()) => {
                progress.sent += body;
                Some(code)
            }
            Err(write_error) => {
                warn!("failed to send error response downstream: {write_error}");
                None
            }
        }
    }

    pub(super) fn finish(
        &self,
        ctx: &mut HttpRequestContext,
        forward: &Forward<'_>,
        error: Option<&Error>,
        progress: &Progress,
    ) {
        let request = Proxied {
            idempotency: self.idempotency_of(ctx, forward.buffer, forward.head),
            id: forward.id,
            id_generated: forward.request.client_id.is_none(),
            method: forward.method,
            route: forward.route,
            protocol: HTTP1,
        };
        let ended = Instant::now();
        self.account(ctx, &request, error, progress, ended);
    }
}

/// The error response a request that ended in `error` gets: its status, its bytes and the length of its body. `Err`
/// when none is sent: the status of the response that went out already, if there was one.
pub(in crate::http::proxy) fn failure_response(
    ctx: &mut HttpRequestContext,
    error: &Error,
    id: &HeaderValue,
    is_head: bool,
    sent_status: Option<u16>,
) -> Result<(u16, BytesMut, usize), Option<u16>> {
    let (code, reason) = error_answer(ctx, error, sent_status)?;
    let rendered = error_response(code, error, reason, ctx.pool.as_deref(), Some(id), is_head)
        .and_then(|(mut response, body)| {
            let mut head = BytesMut::new();
            render_final_response_head(&mut response, false, &mut head)?;
            head.put_slice(&body);
            Ok((head, body.len()))
        });
    match rendered {
        Ok((response, body)) => Ok((code, response, body)),
        Err(render_error) => {
            warn!("failed to send error response downstream: {render_error}");
            Err(None)
        }
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
