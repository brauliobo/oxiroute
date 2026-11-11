//! The direct lane: a fixed response with the fields `write_local_response` gives it in the classic session (the
//! configured ones, `Content-Length`, the request id, `Date`), written without a request context or a header map.
//! A response without a body ends on its head's frame.

use std::time::Instant;

use bytes::Bytes;
use http::header::CONTENT_LENGTH;
use pingora::{
    Error, ErrorType,
    protocols::http::{date::get_cached_date, v2::server::H2Stream},
};

use super::{claim::Plan, relay};
use crate::{
    HttpOperationResult,
    http::{action::FixedResponsePlan, proxy::lane::LaneFront},
};

pub(super) async fn serve(front: &LaneFront, mut stream: H2Stream, plan: Plan) {
    let started = Instant::now();
    let response = plan
        .route
        .action
        .direct_response()
        .expect("only direct routes reach the direct lane");
    let sent = send(front, &mut stream, response, &plan).await.is_ok();
    let body = if plan.is_head() {
        0
    } else {
        response.body.len() as u64
    };
    let _ = front.metrics.record_bytes_sent(body);
    let status = sent.then_some(response.status);
    let _ = front
        .metrics
        .record_http_operation(HttpOperationResult::from_status(status), started.elapsed());
}

async fn send(
    front: &LaneFront,
    stream: &mut H2Stream,
    response: &FixedResponsePlan,
    plan: &Plan,
) -> Result<(), Box<Error>> {
    let mut digits = [0_u8; 20];
    let length = decimal(&mut digits, response.body.len());
    let date = get_cached_date();
    let id = plan.id.as_bytes();
    // The classic head sets `Content-Length` where a configured one is, or appends it; then the id and the date.
    let has_length = response
        .headers
        .iter()
        .any(|(name, _)| name == CONTENT_LENGTH);
    let own = response.headers.iter().map(|(name, value)| {
        let value = if name == CONTENT_LENGTH {
            length
        } else {
            value.as_bytes()
        };
        (name.as_str().as_bytes(), value)
    });
    let appended = (!has_length).then_some((b"content-length".as_slice(), length));
    let fields = own.chain(appended).chain([
        (b"x-request-id".as_slice(), id),
        (b"date".as_slice(), date.as_bytes()),
    ]);
    let body = if plan.is_head() {
        Bytes::new()
    } else {
        response.body.clone()
    };
    let rest = stream
        .send_response_with_data(response.status, fields, body.clone())
        .map_err(|error| {
            Error::because(
                ErrorType::WriteError,
                "while writing h2 response to downstream",
                error,
            )
            .into_down()
        })?;
    if rest > 0 {
        relay::send_body(
            stream,
            body.slice(body.len() - rest..),
            true,
            front.timeouts.session_write(),
        )
        .await?;
    }
    Ok(())
}

/// The decimal digits of `value`, written at the end of `digits`.
fn decimal(digits: &mut [u8; 20], mut value: usize) -> &[u8] {
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            return &digits[start..];
        }
    }
}
