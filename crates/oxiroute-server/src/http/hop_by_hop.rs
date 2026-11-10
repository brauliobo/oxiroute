//! Hop-by-hop fields (RFC 9110 section 7.6.1): what a proxy removes from a message before it forwards it.
//!
//! The fields are `Connection`, `Keep-Alive`, `Proxy-Connection`, `Proxy-Authenticate`, `Proxy-Authorization`,
//! `TE`, `Trailer`, `Transfer-Encoding` and `Upgrade`, and every field a `Connection` option names. They describe
//! one connection, so they never go on to the next one, in either direction; what does survive is decided here
//! once for requests and for responses.
//!
//! A `Connection` option that names a field the message framing or the routing depends on (`Content-Length`,
//! `Transfer-Encoding`, `Host`) is refused: removing it would make the next hop read the message differently
//! from this one. An option is not a field name unless it is a token, so `Connection: two words` is refused too;
//! empty list elements are skipped as RFC 9110 section 5.6.1 asks.

use http::{
    HeaderMap, HeaderName, HeaderValue, Version,
    header::{
        CONNECTION, CONTENT_LENGTH, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER,
        TRANSFER_ENCODING, UPGRADE,
    },
};
use pingora::{
    http::RequestHeader,
    protocols::http::v1::common::{RequestFacts, is_hop_by_hop},
};

const KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
const PROXY_CONNECTION: HeaderName = HeaderName::from_static("proxy-connection");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvalidNomination;

/// What the `Connection` fields of a message say.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ConnectionOptions {
    pub(crate) close: bool,
    pub(crate) upgrade: bool,
    /// The fields the other options name; `keep-alive` is not among them, as that field is always removed.
    pub(crate) nominated: Vec<HeaderName>,
}

impl ConnectionOptions {
    pub(crate) fn parse(headers: &HeaderMap) -> Result<Self, InvalidNomination> {
        let mut options = Self::default();
        for value in headers.get_all(CONNECTION) {
            let value = value.to_str().map_err(|_| InvalidNomination)?;
            for option in value
                .split(',')
                .map(str::trim)
                .filter(|option| !option.is_empty())
            {
                options.add(option)?;
            }
        }
        Ok(options)
    }

    fn add(&mut self, option: &str) -> Result<(), InvalidNomination> {
        if option.eq_ignore_ascii_case("close") {
            self.close = true;
        } else if option.eq_ignore_ascii_case("upgrade") {
            self.upgrade = true;
        } else if !option.eq_ignore_ascii_case("keep-alive") {
            let name = HeaderName::from_bytes(option.as_bytes()).map_err(|_| InvalidNomination)?;
            if matches!(name, CONTENT_LENGTH | TRANSFER_ENCODING | HOST) {
                return Err(InvalidNomination);
            }
            self.nominated.push(name);
        }
        Ok(())
    }
}

/// The `Connection` value a forwarded handshake or close keeps: only the options the next hop must see.
fn remaining_options(close: bool, handshake: bool) -> Option<&'static str> {
    match (close, handshake) {
        (false, false) => None,
        (true, false) => Some("close"),
        (false, true) => Some("Upgrade"),
        (true, true) => Some("close, Upgrade"),
    }
}

/// Removes the hop-by-hop fields of a client request, `Connection` options and the fields they nominate
/// included, before anything else reads or adds a header: a field the proxy adds afterwards cannot be nominated
/// away. `Connection` stays only as `close`, which the downstream session keeps reading, and as `Upgrade` of a
/// valid handshake (an HTTP/1.1 request whose `Upgrade` is named by its `Connection`); `Upgrade` of any other
/// request goes. `TE: trailers` of an HTTP/2 request stays for an HTTP/2 upstream, which carries trailers; the
/// upstream hook removes it for an HTTP/1.1 one. `Transfer-Encoding` is the request's framing and stays.
///
/// Returns the facts of the request after the change. Nothing changes when the request is refused.
pub(crate) fn strip_request(
    request: &mut RequestHeader,
    facts: RequestFacts,
    http2: bool,
) -> Result<RequestFacts, InvalidNomination> {
    if http2 {
        return Ok(strip_http2_request(request, facts));
    }
    if !(facts.hop_by_hop || facts.connection || facts.upgrade) {
        return Ok(facts);
    }
    if let Some(stripped) = strip_usual_connection(request, facts) {
        return Ok(stripped);
    }
    let ConnectionOptions {
        close,
        upgrade,
        nominated,
    } = ConnectionOptions::parse(&request.headers)?;
    let handshake = upgrade && facts.upgrade && request.version == Version::HTTP_11;
    let fixed = if facts.hop_by_hop {
        hop_by_hop_names(request)
    } else {
        Vec::new()
    };
    for name in nominated.iter().chain(&fixed) {
        request.remove_header(name);
    }
    if facts.upgrade && !handshake {
        request.remove_header(&UPGRADE);
    }
    if facts.connection {
        request.remove_header(&CONNECTION);
    }
    let remaining = remaining_options(close, handshake);
    if let Some(value) = remaining {
        request
            .insert_header(CONNECTION, HeaderValue::from_static(value))
            .map_err(|_| InvalidNomination)?;
    }
    Ok(RequestFacts {
        connection: remaining.is_some(),
        upgrade: handshake,
        hop_by_hop: false,
        ..facts
    })
}

fn hop_by_hop_names(request: &RequestHeader) -> Vec<HeaderName> {
    request
        .headers
        .keys()
        .filter(|name| is_hop_by_hop(name))
        .cloned()
        .collect()
}

/// HTTP/2 refuses `Connection`, `Keep-Alive`, `Proxy-Connection`, `Transfer-Encoding` and `Upgrade` before a request
/// reaches the proxy (RFC 9113 section 8.2.2) and `TE` other than `trailers`, so one look at the names finds what
/// is left; `TE: trailers` stays for an HTTP/2 upstream.
fn strip_http2_request(request: &mut RequestHeader, facts: RequestFacts) -> RequestFacts {
    for name in hop_by_hop_names(request).iter().filter(|name| **name != TE) {
        request.remove_header(name);
    }
    facts
}

/// A request whose only hop-by-hop field is the `Connection` of an ordinary client (`keep-alive`, or `close`, which
/// the downstream session reads) without a look at the other fields.
fn strip_usual_connection(
    request: &mut RequestHeader,
    facts: RequestFacts,
) -> Option<RequestFacts> {
    if facts.hop_by_hop || facts.upgrade || !facts.connection {
        return None;
    }
    let mut values = request.headers.get_all(CONNECTION).iter();
    let value = values.next()?.as_bytes();
    if values.next().is_some() {
        return None;
    }
    if value.eq_ignore_ascii_case(b"close") {
        return Some(facts);
    }
    if !value.eq_ignore_ascii_case(b"keep-alive") {
        return None;
    }
    request.remove_header(&CONNECTION);
    Some(RequestFacts {
        connection: false,
        ..facts
    })
}

/// The fields a response, or a request that is rebuilt for another protocol, loses before it goes on (a `101`
/// keeps them, as its handshake is theirs): the fixed hop-by-hop ones and those `Connection` nominates, only the
/// ones that are present.
pub(crate) fn hop_by_hop_removals(
    headers: &HeaderMap,
) -> Result<Vec<HeaderName>, InvalidNomination> {
    let nominated = ConnectionOptions::parse(headers)?.nominated;
    let fixed = [
        CONNECTION,
        KEEP_ALIVE,
        PROXY_AUTHENTICATE,
        PROXY_AUTHORIZATION,
        PROXY_CONNECTION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ];
    Ok(nominated
        .into_iter()
        .chain(fixed)
        .filter(|name| headers.contains_key(name))
        .collect())
}

/// Whether a message has a field [`hop_by_hop_removals`] may remove.
pub(crate) fn has_hop_by_hop(headers: &HeaderMap) -> bool {
    headers.keys().any(|name| {
        is_hop_by_hop(name) || matches!(*name, CONNECTION | TRANSFER_ENCODING | UPGRADE)
    })
}

#[cfg(test)]
mod tests;
