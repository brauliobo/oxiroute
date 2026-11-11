//! The response head the lane writes for a head the codec scanned: what the classic response filter does to it, in the
//! order it does it, and the facts of it reported to the session.

use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{HeaderValue, StatusCode, Version};
use oxiroute_h1_head::{
    Automatic, Class, Connection, Edits, HOP_KEEP_ALIVE, Name, ResponseHead, ScanResponse, Value,
    names, scan_response, write_response,
};
use pingora::protocols::http::{
    date::get_cached_date_at,
    v1::client::{FastFraming, FastHead, HttpSession},
};

use super::{
    buffers,
    proxy::{Forward, Framing},
    rules,
};
use crate::{
    clock::wall_ms,
    http::proxy::{QUEUE_POSITION_HEADER, QUEUE_WAIT_HEADER},
    upstream_peer::POOL_UNAVAILABLE_HEADER,
};

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

/// Serves a response head the codec scans and knows: the status, the framing and whether the origin refused the
/// request as a pool without servers. `None` leaves the head to the classic parser.
pub(super) fn serve_fast(
    out: &mut Vec<u8>,
    raw: &Bytes,
    session: &mut HttpSession,
    forward: &Forward<'_>,
    keepalive: bool,
    (now, queue): (Instant, Option<(u64, u64)>),
) -> Option<(u16, Framing, bool, bool)> {
    let mut head = buffers::take_response_head();
    let served =
        (scan_response(raw, &mut head) == ScanResponse::Head && is_plain(&head)).then(|| {
            let framing = render_fast(out, raw, &head, session, forward, keepalive, (now, queue));
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
    (now, queue): (Instant, Option<(u64, u64)>),
) -> Framing {
    let facts = head.facts;
    let is_head = forward.request.is_head;
    let queue_names = [QUEUE_POSITION_HEADER, QUEUE_WAIT_HEADER];
    let mut queue_digits = [[0_u8; 20]; 2];
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
    // The wait a request had for capacity, for a route that says so, goes after the rules as the classic filter has it.
    if let Some((queue_position, queue_wait)) = queue {
        for ((name, digits), value) in queue_names
            .iter()
            .zip(&mut queue_digits)
            .zip([queue_position, queue_wait])
        {
            let name = Name::new(name.as_str().as_bytes()).expect("a queue header name");
            edit(edits.set(name, Value::trusted(decimal(digits, value))));
        }
    }
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

/// The decimal digits of `value`, written at the end of `digits`.
fn decimal(digits: &mut [u8; 20], mut value: u64) -> &[u8] {
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
