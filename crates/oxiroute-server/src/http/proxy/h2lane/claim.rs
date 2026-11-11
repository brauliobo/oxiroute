//! The decision: whether the lane serves a request, taken without waiting, from what the accept loop knows of it.

use std::{net::IpAddr, sync::Arc};

use http::{HeaderValue, Method, uri::Authority};
use oxiroute_acme::ChallengeStore;
use oxiroute_config::is_unambiguous_http_path;
use pingora::{
    apps::{ClaimedStream, Http2Claim},
    http::RequestHeader,
    protocols::{Digest, http::v2::server::H2Stream},
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
    request: RequestHeader,
    stream: H2Stream,
    digest: &Digest,
) -> Http2Claim {
    let Some(plan) = plan(&front, &request, &stream, digest) else {
        return Http2Claim::Declined { request, stream };
    };
    // The request is read: its header map is not needed any more.
    drop(request);
    Http2Claim::Claimed(ClaimedStream::new(move || serve(front, stream, plan)))
}

/// `None` when the classic path must see the request: it has a body, the route is not the lane's, or its fields are
/// not of the shape the lane writes (see `request::read`).
fn plan(
    front: &LaneFront,
    request: &RequestHeader,
    stream: &H2Stream,
    digest: &Digest,
) -> Option<Plan> {
    if request.method == Method::CONNECT
        || !stream.is_end_stream()
        || front.service.access_log().is_some()
    {
        return None;
    }
    let authority = request.uri.authority()?;
    let path = request.uri.path();
    if !is_unambiguous_http_path(path)
        || (front.challenge.is_some() && ChallengeStore::serves(path))
    {
        return None;
    }
    let route = front
        .service
        .select_route_path(Some(authority), path, &request.method)
        .filter(|route| matches!(route.lane, LaneRoute::Direct | LaneRoute::Proxy))?;
    let client_ip = client_ip(digest);
    let (id, wire, values, replay_safe) = match &route.action {
        HttpActionPlan::Proxy(proxy) => {
            let (id, wire) = request::read(request, authority)?;
            let https = digest.ssl_digest.is_some();
            let Some(values) = decide(
                https,
                &route,
                (&wire.text, client_ip),
                &wire.head,
                authority,
            ) else {
                wire.recycle();
                return None;
            };
            let replay_safe = proxy.policy.request_is_replay_safe(
                &request.method,
                &request.headers,
                true,
                route.policy.request_buffering,
            );
            (id, Some(wire), values, replay_safe)
        }
        _ => (
            request::request_id(&request.headers),
            None,
            rules::Evaluated::default(),
            false,
        ),
    };
    Some(Plan {
        route,
        authority: authority.clone(),
        method: request.method.clone(),
        client_ip,
        id,
        wire,
        values,
        replay_safe,
    })
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
