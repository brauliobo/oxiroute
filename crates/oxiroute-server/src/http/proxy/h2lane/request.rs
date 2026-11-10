//! What the lane reads of an HTTP/2 request: the only module that knows how the request is stored (today the
//! `RequestHeader` the accept loop built; the borrowed request view of the HTTP/2 session replaces it here).
//!
//! The request is written once as the HTTP/1 head the classic path would send, before the edits that depend on the
//! selected server (`Host`, `Connection`): fields in the order the classic header map holds them, `Host` last, as
//! `set_h1_host_from_authority` appends it. That text goes through the scanner of the HTTP/1 lane, so what the
//! codec refuses is not the lane's (a `Host`, `TE`, `Trailer`, `Expect` or `Proxy-*` field, a field value with
//! whitespace around it, a target the codec would write differently).

use http::{HeaderMap, HeaderValue, uri::Authority};
use oxiroute_h1_head::RequestHead;
use pingora::http::RequestHeader;

use crate::http::{
    proxy::lane::{buffers, head},
    request_id::{REQUEST_ID_HEADER, generated_request_id_bytes, resolve_request_id},
};

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

/// The id the request goes on with, and the wire of the request; `None` when the lane does not serve it.
pub(super) fn read(request: &RequestHeader, authority: &Authority) -> Option<(HeaderValue, Wire)> {
    let mut text = buffers::take_write();
    let mut scanned = buffers::take_head();
    let read = write_text(&mut text, request, authority)
        .then(|| head::scan(&text, &mut scanned))
        .flatten();
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

/// `METHOD target HTTP/1.1`, the fields and the `Host` of the authority. `false` for a field value the scanner
/// would trim: the classic path writes it as it is.
fn write_text(out: &mut Vec<u8>, request: &RequestHeader, authority: &Authority) -> bool {
    let Some(target) = request.uri.path_and_query() else {
        return false;
    };
    out.extend_from_slice(request.method.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(target.as_str().as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    for (name, value) in &request.headers {
        let value = value.as_bytes();
        if value
            .first()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            || value
                .last()
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            return false;
        }
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"host: ");
    out.extend_from_slice(authority.as_str().as_bytes());
    out.extend_from_slice(b"\r\n\r\n");
    true
}

/// The id of a request that is not written to an origin: its one valid id, else a generated one.
pub(super) fn request_id(headers: &HeaderMap) -> HeaderValue {
    resolve_request_id(headers.get(&REQUEST_ID_HEADER))
}
