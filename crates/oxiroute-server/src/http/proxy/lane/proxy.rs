//! The proxied lane: a request is forwarded to an HTTP/1.1 server of a pool on the connection itself, with its body,
//! which is streamed to the origin as it arrives when it did not come with the head (`upload`). The head goes upstream and the response head comes back through the head codec,
//! without a header map on either side; a response head the codec does not know is read by the classic client parser, so the response is
//! always served. Selection, leases, timeouts, health signals, evacuation and accounting are the classic request
//! context's own steps.

mod finish;
mod open;
mod relay;
mod serve;
mod wire;

use std::{sync::Arc, time::Instant};

use http::{HeaderValue, Method, uri::Authority};
use oxiroute_config::HttpUpstreamHost;
use oxiroute_h1_head::RequestHead;
use pingora::protocols::{Stream, http::v1::client::HttpSession};

pub(in crate::http::proxy) use self::finish::{error_answer, failure_response};
use super::{buffers, head::Request, rules, upload::Upload};
use crate::{
    RuntimeEndpoint,
    http::{
        action::{HttpActionPlan, HttpRoutePlan, ProxyActionPlan, ProxyPolicyPlan},
        proxy::{attempt::AttemptTimeouts, selected_upstream_host},
    },
    routing::FirstHeader,
    upstream_peer::ReusePeer,
};

/// The log target of the debug line that says a request was served by the lane, with its id: how a test (or an
/// operator who raises the level of this target) tells the lane from the classic path.
pub(crate) const LOG_TARGET: &str = "oxiroute::lane";

/// A body piece of at least this size is written as it was read; smaller ones are copied behind the head.
const DIRECT_BODY: usize = 16 * 1024;

/// How the body of the response goes to the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::http::proxy) enum Framing {
    Length,
    Chunked,
    Empty,
}

/// An origin response whose head is ready to go to the client.
struct Opened {
    // Boxed: the session is 640 bytes and every `.await` that returns it moves it into the caller's future.
    session: Box<HttpSession>,
    /// Where the connection goes back to when the response is served.
    release: ReusePeer,
    head: Vec<u8>,
    framing: Framing,
    status: u16,
    keepalive: bool,
    /// The whole body arrived with the head, so the first read of it does not wait.
    buffered: bool,
    /// What is left of the request body, which goes on while the response is relayed.
    upload: Upload,
    /// What the attempt allows each step of the response.
    timeouts: AttemptTimeouts,
}

/// What an exchange has moved so far.
pub(in crate::http::proxy) struct Progress {
    /// Request body bytes written to the origin.
    pub(in crate::http::proxy) received: usize,
    /// Response body bytes written to the client.
    pub(in crate::http::proxy) sent: usize,
    /// The status that went out.
    pub(in crate::http::proxy) status: Option<u16>,
    pub(in crate::http::proxy) interim: bool,
}

pub(in crate::http::proxy) struct Forward<'a> {
    /// The clock reading at which the request arrived.
    pub(in crate::http::proxy) started: Instant,
    pub(in crate::http::proxy) buffer: &'a [u8],
    pub(in crate::http::proxy) method: &'a Method,
    pub(in crate::http::proxy) client_ip: Option<std::net::IpAddr>,
    pub(in crate::http::proxy) head: &'a RequestHead,
    pub(in crate::http::proxy) request: &'a Request,
    pub(in crate::http::proxy) route: &'a Arc<HttpRoutePlan>,
    pub(in crate::http::proxy) id: &'a HeaderValue,
    /// What the route's request header rules do to this request.
    pub(in crate::http::proxy) values: &'a rules::Evaluated,
    /// Whether the connection serves another request after this one, as far as the request knows.
    pub(in crate::http::proxy) keepalive: bool,
}

pub(super) fn client_ip(stream: &Stream) -> Option<std::net::IpAddr> {
    stream.get_socket_digest().and_then(|digest| {
        digest
            .peer_addr()
            .and_then(|address| address.as_inet().map(std::net::SocketAddr::ip))
    })
}

/// The values of the request header rules of the route for a request, or `None` when the classic path has to answer
/// it.
pub(in crate::http::proxy) fn decide(
    https: bool,
    route: &HttpRoutePlan,
    (buffer, client_ip): (&[u8], Option<std::net::IpAddr>),
    head: &RequestHead,
    authority: &Authority,
) -> Option<rules::Evaluated> {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        unreachable!("only proxied routes reach the proxied lane");
    };
    if !route.edits.computes_request_values() {
        return Some(rules::Evaluated::default());
    }
    let source = rules::Source {
        buffer,
        head,
        authority,
        client_ip,
        https,
    };
    rules::evaluate(&route.edits, &proxy.policy, &source, buffers::take_write()).ok()
}

impl Forward<'_> {
    pub(super) fn policy(&self) -> &ProxyPolicyPlan {
        &proxy_route(self.route).policy
    }
}

/// The proxy action of a route the lane serves as a proxy.
pub(in crate::http::proxy) fn proxy_route(route: &HttpRoutePlan) -> &ProxyActionPlan {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        unreachable!("only proxied routes reach the proxied lane");
    };
    proxy
}

/// The `Host` an origin gets for the selected endpoint, when the route does not preserve the client's.
pub(in crate::http::proxy) fn upstream_host(
    endpoint: &RuntimeEndpoint,
    policy: &HttpUpstreamHost,
    authority: Option<&Authority>,
) -> pingora::Result<Option<HeaderValue>> {
    match policy {
        HttpUpstreamHost::PreserveIncoming => Ok(None),
        policy => selected_upstream_host(endpoint, policy, authority),
    }
}

/// What a request context needs of the request that arrived, whatever protocol it arrived by.
pub(in crate::http::proxy) struct Arrival<'a> {
    pub(in crate::http::proxy) started: Instant,
    /// The route, the id of the request and its authority.
    pub(in crate::http::proxy) route: (&'a Arc<HttpRoutePlan>, &'a HeaderValue, Authority),
    /// The method, the host, the path and the client's address.
    pub(in crate::http::proxy) line: (
        &'a Method,
        Option<&'a str>,
        &'a str,
        Option<std::net::IpAddr>,
    ),
    /// The fields of the request, as a queue tag reads them.
    pub(in crate::http::proxy) fields: Scanned<'a>,
    /// Whether the request could be sent again: an operator's move of it needs that.
    pub(in crate::http::proxy) replay_safe: bool,
    pub(in crate::http::proxy) connection: Option<crate::clients::Counter>,
}

/// The scanned head of a request with the bytes it was scanned from, for what a queue tag reads of it.
pub(in crate::http::proxy) struct Scanned<'a>(
    pub(in crate::http::proxy) &'a [u8],
    pub(in crate::http::proxy) &'a RequestHead,
);

impl FirstHeader for Scanned<'_> {
    fn first(&self, name: &http::HeaderName) -> Option<&str> {
        let (buffer, name) = (self.0, name.as_str().as_bytes());
        let header = self
            .1
            .headers()
            .iter()
            .find(|header| header.name(buffer).eq_ignore_ascii_case(name))?;
        // What `HeaderValue::to_str` accepts: visible ASCII and tabs.
        let value = header.value(buffer);
        value
            .iter()
            .all(|byte| matches!(byte, b' '..=b'~' | b'\t'))
            .then(|| std::str::from_utf8(value).expect("visible ASCII is UTF-8"))
    }
}
