//! The decision: whether the lane serves a request, taken without waiting, from what the accept loop knows of it.

use std::{net::IpAddr, sync::Arc};

use http::{HeaderValue, Method, uri::Authority};
use oxiroute_acme::ChallengeStore;
use pingora::{
    apps::{ClaimedStream, Http2Claim},
    protocols::{
        Digest,
        http::{
            v1::client::FastFraming,
            v2::server::{H2Request, H2Stream},
        },
    },
};

use super::{direct, exchange, request};
use crate::http::{
    action::{HttpActionPlan, HttpRoutePlan},
    proxy::lane::{LaneFront, LaneRoute, buffers, proxy::decide, rules},
};

/// The body that follows the head of a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Body {
    None,
    /// The request announces its length (`content-length`, at least 1).
    Length(usize),
    /// The request has a body and no announced length: the origin gets it chunked, as the classic proxy sends it.
    Unknown,
}

impl Body {
    /// The length the request's fields announce: none for a body without one.
    pub(super) const fn announced(self) -> usize {
        match self {
            Self::Length(length) => length,
            Self::None | Self::Unknown => 0,
        }
    }

    /// How the origin's session writes the body.
    pub(super) const fn framing(self) -> FastFraming {
        FastFraming {
            chunked: matches!(self, Self::Unknown),
            content_length: match self {
                Self::Length(length) => Some(length),
                Self::None | Self::Unknown => None,
            },
        }
    }
}

/// A request the lane serves: the route it matched and what the exchange needs of the request.
pub(super) struct Plan {
    pub(super) route: Arc<HttpRoutePlan>,
    pub(super) authority: Authority,
    pub(super) method: Method,
    pub(super) client_ip: Option<IpAddr>,
    pub(super) id: HeaderValue,
    /// The id is one the proxy made, not the client's.
    pub(super) id_generated: bool,
    /// The request as the HTTP/1 origin gets it, for a proxied route, and what the route's header rules make of it.
    pub(super) wire: Option<request::Wire>,
    pub(super) values: rules::Evaluated,
    /// Whether the request could be sent again, which decides whether an operator may move it to another server.
    pub(super) replay_safe: bool,
    pub(super) body: Body,
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

/// The body that follows the head: none when the head ends the stream, else the length the request announces or, with
/// no length, an open body. `None` for a body the lane does not take: one announced empty yet not ended.
fn body_of(request: &H2Request) -> Option<Body> {
    match (request.is_end_stream(), request.content_length()) {
        (true, None | Some(0)) => Some(Body::None),
        (false, Some(length @ 1..)) => usize::try_from(length).ok().map(Body::Length),
        (false, None) => Some(Body::Unknown),
        _ => None,
    }
}

/// `None` when the classic path must see the request: its body is not one the lane streams, the route is not the
/// lane's, or its fields are not of the shape the lane writes (see `request::read`).
fn plan(front: &LaneFront, request: &H2Request, digest: &Digest) -> Option<Plan> {
    let body = body_of(request)?;
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
    let (id, id_generated, wire, values, replay_safe) =
        if let HttpActionPlan::Proxy(proxy) = &route.action {
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
                body == Body::None,
                route.policy.request_buffering,
            );
            let id_generated = wire.request.client_id.is_none();
            (id, id_generated, Some(wire), values, replay_safe)
        } else {
            let (id, generated) = request::request_id(request.fields());
            (id, generated, None, rules::Evaluated::default(), false)
        };
    Some(Plan {
        route,
        authority,
        method,
        client_ip,
        id,
        id_generated,
        wire,
        values,
        replay_safe,
        body,
    })
}

/// Whether the route takes the body: only a proxied route forwards one, and an announced length over the route's
/// limit is the classic path's `413`. A body of unknown length is held to the limit as it arrives.
fn streams_body(route: &HttpRoutePlan, body: Body) -> bool {
    match body {
        Body::None => true,
        Body::Length(length) => {
            route.lane == LaneRoute::Proxy && !route.policy.exceeds_body_limit(length as u64)
        }
        Body::Unknown => route.lane == LaneRoute::Proxy,
    }
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
