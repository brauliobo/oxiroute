//! The decision: whether the lane serves a request, taken without waiting, from what the accept loop knows of it.

use std::{net::IpAddr, sync::Arc};

use http::{HeaderValue, Method, uri::Authority};
use oxiroute_acme::ChallengeStore;
use pingora::{
    apps::{ClaimedStream, Http2Claim},
    protocols::{
        Digest,
        http::v2::server::{H2Request, H2Stream},
    },
};

use super::{direct, exchange, request};
use crate::http::{
    action::{HttpActionPlan, HttpRoutePlan},
    proxy::lane::{LaneFront, LaneRoute, buffers, proxy::decide, rules},
};

/// A request the lane serves: the route it matched and what the exchange needs of the request.
pub(super) struct Plan {
    pub(super) route: Arc<HttpRoutePlan>,
    pub(super) authority: Authority,
    pub(super) method: Method,
    pub(super) client_ip: Option<IpAddr>,
    pub(super) id: HeaderValue,
    /// The request as the HTTP/1 origin gets it, for a proxied route, and what the route's header rules make of it.
    pub(super) wire: Option<request::Wire>,
    pub(super) values: rules::Evaluated,
    /// Whether the request could be sent again, which decides whether an operator may move it to another server.
    pub(super) replay_safe: bool,
    /// The length of the body that follows the head.
    pub(super) body: usize,
}

impl Plan {
    /// Gives the buffers of the request back to the thread's pools.
    pub(super) fn recycle(self) {
        if let Some(wire) = self.wire {
            wire.recycle();
        }
        buffers::give_write(self.values.into_buffer());
    }

    pub(super) fn is_head(&self) -> bool {
        self.method == Method::HEAD
    }
}

pub(super) fn claim(
    front: Arc<LaneFront>,
    request: H2Request,
    stream: H2Stream,
    digest: &Digest,
) -> Http2Claim {
    let Some(plan) = plan(&front, &request, digest) else {
        return Http2Claim::Declined { request, stream };
    };
    // The request is read: its arena goes back to the pool.
    drop(request);
    Http2Claim::Claimed(ClaimedStream::new(move || serve(front, stream, plan)))
}

/// The length of the body that follows the head: none when the head ends the stream, else the length the request
/// announces. `None` for a body the lane does not take: its length is not announced, or it is announced empty.
fn body_length(request: &H2Request) -> Option<usize> {
    match (request.is_end_stream(), request.content_length()) {
        (true, None | Some(0)) => Some(0),
        (false, Some(length @ 1..)) => usize::try_from(length).ok(),
        _ => None,
    }
}

/// `None` when the classic path must see the request: its body is not one the lane streams, the route is not the
/// lane's, or its fields are not of the shape the lane writes (see `request::read`).
fn plan(front: &LaneFront, request: &H2Request, digest: &Digest) -> Option<Plan> {
    if front.service.access_log().is_some() {
        return None;
    }
    let body = body_length(request)?;
    let method = Method::from_bytes(request.method()).ok()?;
    let authority = Authority::try_from(request.authority()?).ok()?;
    let target = request.path();
    let path = std::str::from_utf8(target.split(|byte| *byte == b'?').next()?).ok()?;
    if front.challenge.is_some() && ChallengeStore::serves(path) {
        return None;
    }
    let route = front
        .service
        .select_route_path(Some(authority.as_str()), path, &method)
        .filter(|route| matches!(route.lane, LaneRoute::Direct | LaneRoute::Proxy))
        .filter(|route| streams_body(route, body))
        .cloned()?;
    let client_ip = client_ip(digest);
    let (id, wire, values, replay_safe) = match &route.action {
        HttpActionPlan::Proxy(proxy) => {
            let (id, wire) = request::read(
                (request.method(), target),
                &request.fields(),
                &authority,
                body,
            )?;
            let https = digest.ssl_digest.is_some();
            let Some(values) = decide(
                https,
                &route,
                (&wire.text, client_ip),
                &wire.head,
                &authority,
            ) else {
                wire.recycle();
                return None;
            };
            let replay_safe = proxy.policy.request_is_replay_safe_with(
                &method,
                |name| {
                    request
                        .fields()
                        .find(|(field, _)| *field == name.as_str().as_bytes())
                        .is_some_and(|(_, value)| !value.is_empty())
                },
                body == 0,
                route.policy.request_buffering,
            );
            (id, Some(wire), values, replay_safe)
        }
        _ => (
            request::request_id(request.fields()),
            None,
            rules::Evaluated::default(),
            false,
        ),
    };
    Some(Plan {
        route,
        authority,
        method,
        client_ip,
        id,
        wire,
        values,
        replay_safe,
        body,
    })
}

/// Whether the route takes a body of `body` bytes: only a proxied route forwards one, and a length over the route's
/// limit is the classic path's `413`.
fn streams_body(route: &HttpRoutePlan, body: usize) -> bool {
    body == 0 || (route.lane == LaneRoute::Proxy && !route.policy.exceeds_body_limit(body as u64))
}

fn client_ip(digest: &Digest) -> Option<IpAddr> {
    let address = digest.socket_digest.as_ref()?.peer_addr()?;
    address.as_inet().map(std::net::SocketAddr::ip)
}

/// The log target of the debug line that says a stream was served by the lane, with the id of its request: how a
/// test (or an operator who raises the level of this target) tells the lane from the classic path.
pub(crate) const LOG_TARGET: &str = "oxiroute::h2lane";

async fn serve(front: Arc<LaneFront>, stream: H2Stream, plan: Plan) {
    log::debug!(target: LOG_TARGET, "served by the HTTP/2 lane: request_id={}", plan.id.to_str().unwrap_or_default());
    match plan.route.lane {
        LaneRoute::Direct => direct::serve(&front, stream, plan).await,
        _ => exchange::serve(&front, stream, plan).await,
    }
}
