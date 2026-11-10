//! The request head the origin gets: the request as the classic path forwards it, with the edits of its sequence in
//! order. `request::read` wrote the fields in the order of the classic header map and `Host` last; what is left is
//! the request id (`stamp_request`, before `Host` is appended), the `Host` of the selected server
//! (`set_h1_host_from_authority`, then `upstream_request_filter`) and the `Connection: close` of a pool that never
//! reuses connections.

use http::{HeaderValue, uri::Authority};
use oxiroute_h1_head::{Edits, EditsFull, Value, names, write_request};
use pingora::{Error, ErrorType};

use super::request::Wire;

/// Appends the head to `out`. `host` is the `Host` the selected server is sent; `None` removes it.
pub(super) fn render(
    out: &mut Vec<u8>,
    wire: &Wire,
    id: &HeaderValue,
    host: Option<&HeaderValue>,
    connection_close: bool,
) {
    let edit = |result: Result<(), EditsFull>| result.expect("a request head needs few edits");
    let mut edits = Edits::new();
    edit(edits.remove(names::HOST));
    if !wire.request.id_stamped {
        edit(edits.set(names::X_REQUEST_ID, Value::trusted(id.as_bytes())));
    }
    if let Some(host) = host {
        edit(edits.set(names::HOST, Value::trusted(host.as_bytes())));
    }
    if connection_close {
        edit(edits.set(names::CONNECTION, Value::trusted(b"close")));
    }
    write_request(out, &wire.text, &wire.head, &edits);
}

/// What `reconcile_upstream_authority` refuses of the `Host` a request is sent: a `Host` that is not the request's
/// authority has to be one (the classic path rebuilds the request target from it), and it has no userinfo.
pub(super) fn check_host(host: &HeaderValue, authority: &Authority) -> Result<(), Box<Error>> {
    let host = host.as_bytes();
    let valid = !host.contains(&b'@')
        && (host == authority.as_str().as_bytes() || Authority::try_from(host).is_ok());
    if valid {
        Ok(())
    } else {
        Err(Error::explain(ErrorType::InvalidHTTPHeader, "invalid Host for the origin").into_in())
    }
}
