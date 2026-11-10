//! The direct lane: a fixed response built as `write_local_response` builds it for the classic session, without a
//! request context. The id is stamped on it, then `response::send_local` writes it as the classic path does.

use std::time::Instant;

use http::header::CONTENT_LENGTH;
use pingora::{http::ResponseHeader, protocols::http::v2::server::H2Stream};

use super::{claim::Plan, response};
use crate::{
    HttpOperationResult,
    http::request_id::stamp_response,
    http::{action::FixedResponsePlan, action::HttpActionPlan, proxy::lane::LaneFront},
};

pub(super) async fn serve(front: &LaneFront, mut stream: H2Stream, plan: Plan) {
    let started = Instant::now();
    let HttpActionPlan::Fixed(response) = &plan.route.action else {
        unreachable!("only direct routes reach the direct lane");
    };
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
) -> Result<(), Box<pingora::Error>> {
    let mut head =
        ResponseHeader::build_no_case(response.status, Some(response.headers.len() + 1))?;
    for (name, value) in &response.headers {
        head.append_header(name.clone(), value.clone())?;
    }
    head.insert_header(CONTENT_LENGTH, response.body.len().to_string())?;
    stamp_response(&mut head, &plan.id)?;
    let body = if plan.is_head() {
        bytes::Bytes::new()
    } else {
        response.body.clone()
    };
    response::send_local(stream, head, body, front.timeouts.session_write()).await
}
