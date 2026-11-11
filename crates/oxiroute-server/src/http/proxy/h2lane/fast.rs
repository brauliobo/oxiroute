//! The origin's response head to the HTTP/2 response fields without a header map on the way: the codec scans the
//! head, the edits of the classic sequence (`apply_response_policy`, the queue headers, `stamp_response`, the automatic
//! `Date` of `write_response_header`) are applied to the scan in the order of the classic header map, and the fields
//! are copied once into a pooled arena that the stream encodes from. A head the codec does not know goes the classic
//! way (`exchange::open`).

use std::cell::RefCell;

use bytes::Bytes;
use http::{HeaderValue, StatusCode};
use oxiroute_h1_head::{Edit, HeaderOrder, Items, MAX_HEADERS, ResponseHead, Value, names};
use pingora::protocols::http::date::get_cached_date;

use crate::http::{
    action::ProxyPolicyPlan,
    proxy::lane::{
        response::{QueueValues, filtered},
        rules::RouteEdits,
    },
};

/// Pooled arenas kept per thread, and the largest capacities that are kept.
const POOL_MAX: usize = 64;
const KEEP_ARENA: usize = 16 * 1024;
const KEEP_ENDS: usize = 512;

#[derive(Default)]
struct Buf {
    arena: Vec<u8>,
    ends: Vec<u32>,
}

thread_local! {
    static POOL: RefCell<Vec<Buf>> = const { RefCell::new(Vec::new()) };
}

/// The fields of a response, names lowercase, in the order of the classic header map: one arena that holds every
/// name and value in turn, and where each ends.
pub(super) struct Fields(Buf);

impl Fields {
    fn take() -> Self {
        Self(
            POOL.with(|pool| pool.borrow_mut().pop())
                .unwrap_or_default(),
        )
    }

    /// Appends a field; the name is lowercased on the way.
    fn push(&mut self, name: &[u8], value: &[u8]) {
        let Buf { arena, ends } = &mut self.0;
        arena.extend(name.iter().map(u8::to_ascii_lowercase));
        ends.push(u32::try_from(arena.len()).expect("a head is bounded"));
        arena.extend_from_slice(value);
        ends.push(u32::try_from(arena.len()).expect("a head is bounded"));
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        let mut start = 0;
        let arena = &self.0.arena[..];
        self.0.ends.chunks_exact(2).map(move |ends| {
            let (name_end, value_end) = (ends[0] as usize, ends[1] as usize);
            let field = (&arena[start..name_end], &arena[name_end..value_end]);
            start = value_end;
            field
        })
    }
}

impl Drop for Fields {
    fn drop(&mut self) {
        let mut buf = std::mem::take(&mut self.0);
        if buf.arena.capacity() > KEEP_ARENA || buf.ends.capacity() > KEEP_ENDS {
            return;
        }
        buf.arena.clear();
        buf.ends.clear();
        // A response dropped while the thread exits finds the pool gone: nothing to return to.
        let _ = POOL.try_with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < POOL_MAX {
                pool.push(buf);
            }
        });
    }
}

/// The response fields of a scanned head, in the order the classic header map holds them after the edits.
pub(super) fn fields(
    raw: &Bytes,
    head: &ResponseHead,
    rules: (&RouteEdits, &ProxyPolicyPlan),
    queue: Option<&QueueValues>,
    id: &HeaderValue,
) -> Fields {
    let date = get_cached_date();
    let mut edits = filtered(raw, head, rules, queue, id);
    edits
        .set(names::DATE, Value::trusted(date.as_bytes()))
        .expect("a response head needs few edits");
    let items = Items {
        buf: raw,
        headers: head.headers(),
        edits: edits.as_slice(),
    };
    let order = HeaderOrder::after_edits(&items, head.facts.duplicate_names, head.facts.present);
    let mut fields = Fields::take();
    order.for_each(|item| {
        let item = usize::from(item);
        if item < MAX_HEADERS {
            let header = &head.headers()[item];
            fields.push(header.name(raw), header.value(raw));
        } else {
            let (Edit::Set(name, value) | Edit::Add(name, value)) = items.edits[item - MAX_HEADERS]
            else {
                return;
            };
            fields.push(name.lower(), value.as_bytes());
        }
    });
    fields
}

/// The status of a scanned head.
pub(super) fn status(head: &ResponseHead) -> StatusCode {
    StatusCode::from_u16(head.status).expect("a scanned status")
}
