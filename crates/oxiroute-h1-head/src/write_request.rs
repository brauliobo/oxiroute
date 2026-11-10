//! The request head the fast lane sends upstream.

use crate::edit::Edits;
use crate::emit::{Walk, emit};
use crate::order::{HeaderOrder, Items};
use crate::request::RequestHead;
use alloc::vec::Vec;

/// Appends `METHOD target HTTP/1.1\r\n`, the headers with `edits` applied, and the blank line to `out`, byte for byte
/// what the classic path writes for the same head and the same `HeaderMap` calls: `RequestHeader::build_no_case`,
/// one `append` per header, the edits, then `header_to_h1_wire`.
pub fn write_request(out: &mut Vec<u8>, buf: &[u8], head: &RequestHead, edits: &Edits<'_>) {
    let items = Items {
        buf,
        headers: head.headers(),
        edits: edits.as_slice(),
    };
    // A scanned line grows by at most the space after its colon.
    out.reserve(head.head_len as usize + usize::from(head.count) + edits.added_bytes() + 2);
    out.extend_from_slice(&buf[..head.line_len as usize]);
    if edits.is_empty() && !head.facts.duplicate_names {
        emit(out, &items, Walk::Arrival, 0);
    } else {
        let order =
            HeaderOrder::after_edits(&items, head.facts.duplicate_names, head.facts.present);
        emit(out, &items, Walk::Map(&order), 0);
    }
    out.extend_from_slice(b"\r\n");
}
