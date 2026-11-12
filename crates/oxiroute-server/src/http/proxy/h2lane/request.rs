//! What the lane reads of an HTTP/2 request: the only module that knows how the request is stored (the borrowed
//! request view of the HTTP/2 connection).
//!
//! The request is written once as the HTTP/1 head the classic path would send, before the edits that depend on the
//! selected server (`Host`, `Connection`): fields in the order the classic header map holds them, `Host` last, as
//! `set_h1_host_from_authority` appends it. That text goes through the scanner of the HTTP/1 lane, so what the
//! codec refuses is not the lane's (a `Host`, `TE`, `Trailer`, `Expect` or `Proxy-*` field, a field value with
//! whitespace around it, a target the codec would write differently) and a request that announces a body.

use http::{HeaderValue, uri::Authority};
use oxiroute_h1_head::RequestHead;

use super::claim::Body;
use crate::http::{
    proxy::lane::{buffers, head},
    request_id::{generated_request_id_bytes, resolve_request_id},
};

/// The fields of a request head, as the decoder holds them: lowercase names, wire order.
pub(super) trait Fields<'a>: Iterator<Item = (&'a [u8], &'a [u8])> + Clone {}

impl<'a, T: Iterator<Item = (&'a [u8], &'a [u8])> + Clone> Fields<'a> for T {}

/// The most fields the lane regroups by name; a request with more is the classic path's.
const MAX_FIELDS: usize = 64;

/// The request as an HTTP/1 head, scanned.
pub(super) struct Wire {
    pub(super) text: Vec<u8>,
    pub(super) head: Box<RequestHead>,
    pub(super) request: head::Request,
}

impl Wire {
    /// Gives the buffers back to the thread's pools.
    pub(super) fn recycle(self) {
        buffers::give_write(self.text);
        buffers::give_head(self.head);
    }
}

/// The id the request goes on with, and the wire of the request; `None` when the lane does not serve it. `target` is
/// the request target as sent (path and query); `body` is the body that follows the head, whose length the fields
/// must announce.
pub(super) fn read<'a>(
    (method, target): (&[u8], &[u8]),
    fields: &impl Fields<'a>,
    authority: &Authority,
    body: Body,
) -> Option<(HeaderValue, Wire)> {
    let mut text = buffers::take_write();
    let mut scanned = buffers::take_head();
    let read = write_text(&mut text, (method, target), fields, authority)
        .then(|| head::line(&text).and_then(|line| head::scan_h2(&text, line, &mut scanned)))
        .flatten()
        .filter(|read| read.body_len == body.announced());
    let Some(read) = read else {
        buffers::give_write(text);
        buffers::give_head(scanned);
        return None;
    };
    let generated = generated_request_id_bytes();
    let id = read.client_id.map_or(&generated[..], |span| span.of(&text));
    let id = HeaderValue::from_bytes(id).expect("request ids are visible ASCII");
    Some((
        id,
        Wire {
            text,
            head: scanned,
            request: read,
        },
    ))
}

/// `METHOD target HTTP/1.1`, the fields and the `Host` of the authority. `false` for a request the lane does not
/// write: a field value the scanner would trim, too many fields to regroup.
fn write_text<'a>(
    out: &mut Vec<u8>,
    (method, target): (&[u8], &[u8]),
    fields: &impl Fields<'a>,
    authority: &Authority,
) -> bool {
    out.extend_from_slice(method);
    out.push(b' ');
    out.extend_from_slice(target);
    out.extend_from_slice(b" HTTP/1.1\r\n");
    if fields.clone().count() > MAX_FIELDS {
        return false;
    }
    // The classic header map keeps the fields of one name together, after the first of them.
    for (at, (name, _)) in fields.clone().enumerate() {
        if fields.clone().take(at).any(|(earlier, _)| earlier == name) {
            continue;
        }
        let same = fields.clone().skip(at).filter(|(other, _)| *other == name);
        for (name, value) in same {
            if !write_field(out, name, value) {
                return false;
            }
        }
    }
    out.extend_from_slice(b"host: ");
    out.extend_from_slice(authority.as_str().as_bytes());
    out.extend_from_slice(b"\r\n\r\n");
    true
}

/// `false` for a value the scanner would trim: the classic path writes it as it is.
fn write_field(out: &mut Vec<u8>, name: &[u8], value: &[u8]) -> bool {
    let padded = |byte: Option<&u8>| byte.is_some_and(|byte| matches!(byte, b' ' | b'\t'));
    if padded(value.first()) || padded(value.last()) {
        return false;
    }
    out.extend_from_slice(name);
    out.extend_from_slice(b": ");
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
    true
}

/// The id of a request that is not written to an origin: its first id when that is a valid one, else a generated one
/// (then the flag is set).
pub(super) fn request_id<'a>(mut fields: impl Fields<'a>) -> (HeaderValue, bool) {
    let first = fields
        .find(|(name, _)| *name == b"x-request-id")
        .and_then(|(_, value)| HeaderValue::from_bytes(value).ok());
    let id = resolve_request_id(first.as_ref());
    let generated = first.as_ref() != Some(&id);
    (id, generated)
}
