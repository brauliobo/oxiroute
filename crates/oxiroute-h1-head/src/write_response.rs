//! The response head the fast lane sends to the client.

use crate::class::Class;
use crate::edit::{Edits, EditsFull, Value, names};
use crate::emit::{Walk, emit};
use crate::order::{HeaderOrder, Items};
use crate::response::ResponseHead;
use alloc::vec::Vec;

/// The headers the vendored serializer adds to a final response and writes after all others, replacing any copy of
/// either in the head: `Date: <date>` then `Connection: <connection>`.
#[derive(Clone, Copy, Debug)]
pub struct Automatic<'a> {
    pub date: &'a [u8],
    pub connection: &'a [u8],
}

/// Appends the status line, the headers with `edits` applied, and the blank line to `out`, byte for byte what the
/// classic path writes for the same head and the same `HeaderMap` calls: `ResponseHeader::build_no_case` with the
/// status, version and reason of the head, one `append` per header, the edits, then `http_resp_header_to_buf`.
///
/// `minor` is the version of the status line. With `automatic`, `Date` and `Connection` entries are left out wherever
/// they are and written last, as the serializer does for a response without a case map; without it all headers are
/// written in map order.
pub fn write_response(
    out: &mut Vec<u8>,
    buf: &[u8],
    head: &ResponseHead,
    edits: &Edits<'_>,
    minor: u8,
    automatic: Option<Automatic<'_>>,
) {
    let items = Items {
        buf,
        headers: head.headers(),
        edits: edits.as_slice(),
    };
    let automatic_bytes = automatic.map_or(0, |a| a.date.len() + a.connection.len() + 32);
    out.reserve(
        head.head_len as usize
            + usize::from(head.count)
            + edits.added_bytes()
            + automatic_bytes
            + 2,
    );
    let line = out.len();
    out.extend_from_slice(&buf[..head.line_len as usize]);
    if head.minor != minor {
        out[line + 7] = b'0' + minor;
    }
    let order = HeaderOrder::after_edits(&items, head.facts.duplicate_names, head.facts.present);
    match automatic {
        None => emit(out, &items, Walk::Map(&order), 0),
        Some(auto) => {
            emit(
                out,
                &items,
                Walk::Map(&order),
                Class::Date.bit() | Class::Connection.bit(),
            );
            out.extend_from_slice(b"Date: ");
            out.extend_from_slice(auto.date);
            out.extend_from_slice(b"\r\nConnection: ");
            out.extend_from_slice(auto.connection);
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"\r\n");
}

impl<'a> Edits<'a> {
    /// Gives the response the request's id, as `stamp_response` does: an `X-Request-Id` the upstream sent is removed
    /// and moves to `X-Upstream-Request-Id` when it differs from `id`, and `id` is set last.
    ///
    /// # Errors
    /// [`EditsFull`] when the edits do not fit.
    pub fn stamp_request_id(
        &mut self,
        buf: &'a [u8],
        head: &ResponseHead,
        id: Value<'a>,
    ) -> Result<(), EditsFull> {
        if let Some(upstream) = head.find(Class::XRequestId) {
            self.remove(names::X_REQUEST_ID)?;
            let upstream = Value(upstream.value(buf));
            if upstream != id {
                self.set(names::X_UPSTREAM_REQUEST_ID, upstream)?;
            }
        }
        self.set(names::X_REQUEST_ID, id)
    }
}
