//! The request head the fast lane sends upstream.

use crate::edit::Edits;
use crate::emit::{emit, size_bound};
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
    let order = HeaderOrder::after_edits(&items, head.facts.duplicate_names, head.facts.present);
    out.reserve(head.method.len as usize + head.target.len as usize + size_bound(&items) + 16);
    out.extend_from_slice(head.method(buf));
    out.push(b' ');
    out.extend_from_slice(head.target(buf));
    out.extend_from_slice(b" HTTP/1.1\r\n");
    emit(out, &items, &order, 0);
    out.extend_from_slice(b"\r\n");
}
