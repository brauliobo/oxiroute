use std::{net::IpAddr, sync::Arc, time::Instant};

use http::{HeaderValue, Method};
use log::debug;
use oxiroute_epoll_h1::{AttemptTimeouts, Failure, Request, Reuse, Selected, Target};
use oxiroute_h1_head::RequestHead;
use pingora::{Error, upstreams::peer::Peer as _};

use super::{LOG_TARGET, lease::EngineLease, outcome::Pending, plan::EnginePlan, routes::Routes};
use crate::{
    http::{
        action::HttpRoutePlan,
        proxy::{
            HttpRequestContext,
            attempt::AttemptTimeouts as Allowed,
            lane::{
                LaneFront,
                head::{self, Request as LaneRequest},
                proxy::{Forward, decide, upstream_host},
                render::{HostEdit, host_edit},
                request_id,
                rules::Evaluated,
            },
        },
        request_id::generated_request_id_bytes,
    },
    upstream_peer::POOL_UNAVAILABLE,
};

const POOLED_IDLE: std::time::Duration = std::time::Duration::from_mins(1);

pub(super) struct Exchange {
    pub(super) ctx: Box<HttpRequestContext>,
    pub(super) route: Arc<HttpRoutePlan>,
    pub(super) lane: LaneRequest,
    pub(super) id: HeaderValue,
    pub(super) method: Method,
    pub(super) client_ip: Option<IpAddr>,
    pub(super) values: Evaluated,
    pub(super) idempotency: Option<Box<[u8]>>,
    pub(super) host: HostEdit,
    pub(super) connection_close: bool,
    pub(super) replayable: bool,
    pub(super) reasoned: bool,
    pub(super) pending: Option<Pending>,
    pub(super) error: Option<Box<Error>>,
    pub(super) answered_body: usize,
}

impl Exchange {
    pub(super) fn begin(
        front: &LaneFront,
        routes: &Routes,
        request: &Request<'_>,
        now: Instant,
    ) -> Option<Self> {
        let (buffer, head) = (request.bytes(), request.head());
        let lane = head::adopt(buffer, head)?;
        let method = Method::from_bytes(lane.method(buffer)).ok()?;
        let target = lane.target(buffer)?;
        let (route, authority) = routes.resolve(front, &method, &target)?;
        let connection_close = route.engine.as_ref()?.connection_close;
        if !LaneFront::takes_body(&route, &lane) {
            return None;
        }
        let client_ip = Some(request.peer().ip());
        let values = decide(false, &route, (buffer, client_ip), head, &authority)?;
        let id = request_id(buffer, &lane, generated_request_id_bytes());
        debug!(target: LOG_TARGET, "served by the epoll data plane: request_id={}", id.to_str().unwrap_or_default());
        let ctx = front.begin(
            &Forward {
                started: now,
                buffer,
                method: &method,
                client_ip,
                head,
                request: &lane,
                route: &route,
                id: &id,
                values: &values,
                keepalive: true,
            },
            authority,
            &target,
        );
        let idempotency = front.idempotency_of(&ctx, buffer, head).map(Box::from);
        ctx.bind_evacuation_to_engine(request.waker());
        Some(Self {
            ctx,
            route,
            lane,
            id,
            method,
            client_ip,
            values,
            idempotency,
            host: HostEdit::AsSent,
            connection_close,
            replayable: request.replayable(),
            reasoned: false,
            pending: None,
            error: None,
            answered_body: 0,
        })
    }

    pub(super) fn cancelled(&self) -> bool {
        self.ctx
            .evacuation()
            .is_some_and(crate::http::evacuation::Evacuation::cancelled)
    }

    fn plan(route: &HttpRoutePlan) -> &EnginePlan {
        route
            .engine
            .as_ref()
            .expect("the route was selected for the epoll data plane")
    }

    pub(super) fn forward<'a>(&'a self, buffer: &'a [u8], head: &'a RequestHead) -> Forward<'a> {
        Forward {
            started: self.ctx.started_at,
            buffer,
            method: &self.method,
            client_ip: self.client_ip,
            head,
            request: &self.lane,
            route: &self.route,
            id: &self.id,
            values: &self.values,
            keepalive: true,
        }
    }

    pub(super) fn select(&mut self) -> Selected<EngineLease> {
        if let Some(pending) = self.pending.take() {
            let error = self.retire(pending);
            if !error.retry() {
                return self.refuse(error);
            }
        }
        self.error = None;
        self.ctx.arm_evacuation_for_engine(self.replayable);
        let first = !self.ctx.has_attempted();
        let attempt = &Self::plan(&self.route).attempt;
        match self.ctx.next_attempt_now(attempt, upstream_host) {
            Err(error) => self.refuse(error),
            Ok(None) => self.decline(first),
            Ok(Some(allowed)) => self.attach(&allowed, first),
        }
    }

    fn refuse(&mut self, error: Box<Error>) -> Selected<EngineLease> {
        self.error = Some(error);
        Selected::Refused(Failure::NoServer)
    }

    fn decline(&mut self, first: bool) -> Selected<EngineLease> {
        if !first {
            return self.refuse(Error::new_up(POOL_UNAVAILABLE));
        }
        self.ctx.abandon();
        Selected::Handoff
    }

    fn attach(&mut self, allowed: &Allowed, first: bool) -> Selected<EngineLease> {
        let Some(peer) = self.ctx.selected_peer() else {
            self.ctx.release_lease();
            return self.decline(first);
        };
        let address = peer
            .address()
            .as_inet()
            .copied()
            .expect("a reuse peer has a socket address");
        let lease = self.ctx.take_lease().and_then(EngineLease::new);
        let Some(lease) = lease else {
            return self.decline(first);
        };
        self.host = host_edit(&mut self.ctx);
        let reuse = if self.connection_close {
            Reuse::Never
        } else {
            Reuse::Pooled {
                idle: peer.idle_timeout().unwrap_or(POOLED_IDLE),
            }
        };
        Selected::Server(Target {
            address,
            key: peer.reuse_hash(),
            reuse,
            lease,
            timeouts: AttemptTimeouts {
                connect: allowed.connect(),
                header: allowed.read(),
                body: allowed.body_read(),
                write: allowed.write(),
                deadline: Some(self.ctx.deadline),
            },
        })
    }
}
