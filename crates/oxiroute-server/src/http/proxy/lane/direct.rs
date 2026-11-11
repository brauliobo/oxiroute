//! The direct lane: a fixed response written from the rendered head of its route, as
//! `HttpSession::write_rendered_response` writes it, without a session or a header map.

use bytes::{BufMut as _, BytesMut};
use pingora::protocols::http::date::put_cached_date;

use crate::http::action::FixedResponsePlan;

/// The response: the rendered head, the request id, `Date` and `Connection` when the service writes them, the
/// body unless the request was `HEAD`.
pub(super) fn render(
    out: &mut BytesMut,
    response: &FixedResponsePlan,
    request_id: &[u8],
    automatic: Option<&str>,
    head: bool,
) {
    let rendered = response.rendered_head.as_deref().unwrap_or_default();
    let body = if head { &[][..] } else { &response.body[..] };
    out.reserve(rendered.len() + body.len() + request_id.len() + 128);
    put_head(out, rendered, request_id, automatic);
    out.put_slice(body);
}

/// A rendered head completed as `write_rendered_response` completes it: the request id, `Date` and `Connection`
/// when the service writes them, and the blank line.
pub(super) fn put_head(
    out: &mut BytesMut,
    rendered: &[u8],
    request_id: &[u8],
    automatic: Option<&str>,
) {
    out.put_slice(rendered);
    out.put_slice(b"x-request-id: ");
    out.put_slice(request_id);
    out.put_slice(b"\r\n");
    if let Some(connection) = automatic {
        out.put_slice(b"Date: ");
        put_cached_date(out);
        out.put_slice(b"\r\nConnection: ");
        out.put_slice(connection.as_bytes());
        out.put_slice(b"\r\n");
    }
    out.put_slice(b"\r\n");
}
