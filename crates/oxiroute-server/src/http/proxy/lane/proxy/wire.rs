use std::{pin::pin, time::Instant};

use bytes::{Bytes, BytesMut};
use http::{StatusCode, Version};
use pingora::{
    Error, ErrorType,
    protocols::{
        Stream,
        http::v1::{
            client::{FastFraming, HttpSession},
            server::render_informational_head,
        },
    },
};
use tokio::io::AsyncWriteExt as _;

use super::{Forward, Framing, Progress};
use crate::http::{
    proxy::{
        HttpRequestContext, apply_response_policy,
        attempt::AttemptTimeouts,
        lane::timing::{Expired, Wait, within, within_for as within_step},
        proxy_policy,
    },
    request_id::stamp_response,
};

/// Sends the part of the request body that arrived with the head to the origin; all of it ends the body. Nothing
/// is written for a body that went with the head: the session's body is complete with it.
pub(super) async fn send_arrived(
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
pub(super) async fn read_head(
    ctx: &HttpRequestContext,
    session: &mut HttpSession,
    timeouts: &AttemptTimeouts,
) -> Result<Bytes, Box<Error>> {
    let deadline = tokio::time::Instant::from_std(ctx.now + timeouts.read());
    match within(Wait::Step, deadline, pin!(session.read_raw_head())).await {
        Ok(head) => head.map_err(Error::into_up),
        Err(Expired) => {
            Err(Error::explain(ErrorType::ReadTimedout, "while reading response headers").into_up())
        }
    }
}

/// The next piece of the origin's response body, within the idle time the attempt allows between pieces.
pub(super) async fn read_piece(
    session: &mut HttpSession,
    timeouts: &AttemptTimeouts,
) -> Result<Option<Bytes>, Box<Error>> {
    match within_step(
        Wait::Step,
        Some(timeouts.body_read()),
        pin!(session.read_body_bytes()),
    )
    .await
    {
        Ok(piece) => piece.map_err(Error::into_up),
        Err(Expired) => Err(Error::explain(ErrorType::ReadTimedout, "reading body").into_up()),
    }
}

/// The stream buffers its writes: what a client waits for must be flushed before the next wait on the origin.
pub(super) async fn flush(stream: &mut Stream) -> Result<(), Box<Error>> {
    stream.flush().await.map_err(|error| {
        Error::because(ErrorType::WriteError, "flushing the response", error).into_down()
    })
}

pub(super) async fn write(stream: &mut Stream, bytes: &[u8]) -> Result<(), Box<Error>> {
    stream.write_all(bytes).await.map_err(|error| {
        Error::because(ErrorType::WriteError, "writing the response", error).into_down()
    })
}

/// The final response head of the session's parser; informational heads on the way go to the client filtered and
/// stamped as the classic response filter does.
pub(super) async fn read_final_head(
    stream: &mut Stream,
    ctx: &HttpRequestContext,
    session: &mut HttpSession,
    (forward, progress): (&Forward<'_>, &mut Progress),
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
        progress.interim = true;
        flush(stream).await?;
    }
}

/// What the classic response filter and the proxy do to a head the classic parser read. The framing, and whether the
/// body is delimited by the end of the connection.
pub(super) fn prepare_slow(
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
