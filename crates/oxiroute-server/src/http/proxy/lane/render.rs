//! The origin's request head: the client's, as the classic path forwards it, and a small body that leaves with it.

use http::HeaderValue;
use oxiroute_config::HttpUpstreamHost;
use oxiroute_h1_head::{Connection, Edits, Value, names, write_request};

use super::{proxy::Forward, rules};
use crate::http::proxy::{HttpRequestContext, proxy_policy};

/// What the request sends to the origin as its `Host`.
pub(super) enum HostEdit {
    AsSent,
    Set(HeaderValue),
    Remove,
}

/// A body of at most this many bytes that is whole when the head is written leaves with the origin's head, in one
/// write.
pub(in crate::http::proxy) const SMALL_BODY: usize = 2 * 1024;

/// Adds the body to the origin's request head when it is whole and small; whether it did.
pub(super) fn push_small_body(head: &mut Vec<u8>, forward: &Forward<'_>) -> bool {
    let body = forward.request.body(forward.buffer);
    let small =
        !body.is_empty() && body.len() == forward.request.body_len && body.len() <= SMALL_BODY;
    if small {
        head.extend_from_slice(body);
    }
    small
}

pub(super) fn host_edit(ctx: &mut HttpRequestContext) -> HostEdit {
    if matches!(
        proxy_policy(ctx).upstream_host,
        HttpUpstreamHost::PreserveIncoming
    ) {
        return HostEdit::AsSent;
    }
    ctx.selected_upstream_host
        .take()
        .map_or(HostEdit::Remove, HostEdit::Set)
}

/// The upstream request head: the client's, as the classic path forwards it. The edits are the `HeaderMap` calls of
/// the classic sequence in its order: the `Connection` of a keep-alive client leaves with the hop-by-hop fields, an
/// id is stamped when the request has no single valid one, the host policy applies, a client's `close` is not
/// forwarded and a pool that never reuses connections says `close` itself.
pub(super) fn render_request(
    out: &mut Vec<u8>,
    forward: &Forward<'_>,
    host: &HostEdit,
    connection_close: bool,
) {
    let (buffer, head, request) = (forward.buffer, forward.head, forward.request);
    let connection = head.facts.connection;
    // Declared before the edits, which may borrow it.
    let incoming;
    let mut edits = Edits::new();
    let edit = |result: Result<(), oxiroute_h1_head::EditsFull>| {
        result.expect("a request head needs few edits");
    };
    if connection == Connection::KeepAlive {
        edit(edits.remove(names::CONNECTION));
    }
    if !request.id_stamped {
        edit(edits.set(names::X_REQUEST_ID, Value::trusted(forward.id.as_bytes())));
    }
    match host {
        HostEdit::AsSent => {}
        HostEdit::Set(value) => edit(edits.set(names::HOST, Value::trusted(value.as_bytes()))),
        HostEdit::Remove => edit(edits.remove(names::HOST)),
    }
    if connection == Connection::Close {
        edit(edits.remove(names::CONNECTION));
    }
    if connection_close {
        edit(edits.set(names::CONNECTION, Value::trusted(b"close")));
    }
    // The host a rule may name: the request's own when it is passed on as it is.
    let selected = match host {
        HostEdit::Set(value) => Some(value),
        HostEdit::AsSent if forward.values.names_host() => {
            incoming = HeaderValue::from_bytes(head.host(buffer)).ok();
            incoming.as_ref()
        }
        HostEdit::AsSent | HostEdit::Remove => None,
    };
    let policy = forward.policy();
    edit(rules::edit_request(
        &mut edits,
        &forward.route.edits,
        policy,
        forward.values,
        selected,
    ));
    write_request(out, buffer, head, &edits);
}
