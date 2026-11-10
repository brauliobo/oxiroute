//! The origin's response head to the HTTP/2 response head without a header map on the way in: the codec scans the
//! head, the edits of the classic sequence (`apply_response_policy`, `stamp_response`, the automatic `Date` of
//! `write_response_header`) are applied to the scan in the order of the classic header map, and the fields are
//! appended to the map the stream takes. A head the codec does not know goes the classic way (`exchange::open`).

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use oxiroute_h1_head::{
    Class, Edit, Edits, EditsFull, HeaderOrder, Items, MAX_HEADERS, ResponseHead, Value, names,
};
use pingora::protocols::http::date::get_cached_date;

use crate::http::proxy::lane::proxy::RESPONSE_HOP_BY_HOP;

/// The edits the classic sequence makes to the head, `Date` last.
fn edits<'a>(
    raw: &'a Bytes,
    head: &ResponseHead,
    id: &'a HeaderValue,
    date: &'a HeaderValue,
) -> Edits<'a> {
    let edit = |result: Result<(), EditsFull>| result.expect("a response head needs few edits");
    let facts = head.facts;
    let mut edits = Edits::new();
    if facts.transfer_encoding && head.find(Class::ContentLength).is_some() {
        edit(edits.remove(names::CONTENT_LENGTH));
    }
    for (class, name) in RESPONSE_HOP_BY_HOP {
        if facts.present & class.bit() != 0 {
            edit(edits.remove(name));
        }
    }
    edit(edits.stamp_request_id(raw, head, Value::trusted(id.as_bytes())));
    edit(edits.set(names::DATE, Value::trusted(date.as_bytes())));
    edits
}

/// The response fields of a scanned head, in the order the classic header map holds them after the edits.
pub(super) fn fields(raw: &Bytes, head: &ResponseHead, id: &HeaderValue) -> HeaderMap {
    let date = get_cached_date();
    let edits = edits(raw, head, id, &date);
    let items = Items {
        buf: raw,
        headers: head.headers(),
        edits: edits.as_slice(),
    };
    let order = HeaderOrder::after_edits(&items, head.facts.duplicate_names, head.facts.present);
    let mut map = HeaderMap::with_capacity(usize::from(head.count) + 4);
    order.for_each(|item| {
        let item = usize::from(item);
        let (name, value) = if item < MAX_HEADERS {
            let header = &head.headers()[item];
            let start = header.value_start as usize;
            (
                HeaderName::from_bytes(header.name(raw)),
                HeaderValue::from_maybe_shared(raw.slice(start..start + header.value_len as usize)),
            )
        } else {
            let (Edit::Set(name, value) | Edit::Add(name, value)) = items.edits[item - MAX_HEADERS]
            else {
                return;
            };
            let value = if name.class() == Class::Date {
                Ok(date.clone())
            } else {
                HeaderValue::from_bytes(value.as_bytes())
            };
            (HeaderName::from_bytes(name.lower()), value)
        };
        map.append(
            name.expect("a scanned name is a token"),
            value.expect("a scanned value is a field value"),
        );
    });
    map
}

/// The status of a scanned head.
pub(super) fn status(head: &ResponseHead) -> StatusCode {
    StatusCode::from_u16(head.status).expect("a scanned status")
}
