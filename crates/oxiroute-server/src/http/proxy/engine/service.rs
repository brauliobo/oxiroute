use std::{net::SocketAddr, sync::Arc, time::Instant};

use oxiroute_epoll_h1::{
    Answered, Failure, Handoff, Outcome, Recovery, Request, Response, Routed, Secured, Selected,
    Service, Transfer,
};
use pingora::apps::ServerApp;
use tokio::sync::mpsc::UnboundedSender;

use super::{admission::EngineAdmission, exchange::Exchange, lease::EngineLease, routes::Routes};
use crate::{clients::Protocol, http::proxy::lane::LaneFront};

const TEST_ENGINE_PANIC_ENV: &str = "OXIROUTE_INTERNAL_TEST_ENGINE_PANIC";

pub(super) struct EngineFront<A> {
    app: Arc<A>,
    lane: Arc<LaneFront>,
    routes: Routes,
    handoffs: UnboundedSender<Handoff<EngineAdmission>>,
    terminates_tls: bool,
    inject_panic: bool,
}

impl<A> EngineFront<A> {
    pub(super) fn new(
        app: Arc<A>,
        lane: Arc<LaneFront>,
        handoffs: UnboundedSender<Handoff<EngineAdmission>>,
        terminates_tls: bool,
    ) -> Self {
        Self {
            app,
            lane,
            routes: Routes::new(),
            handoffs,
            terminates_tls,
            inject_panic: std::env::var_os(TEST_ENGINE_PANIC_ENV).is_some(),
        }
    }
}

impl<A: ServerApp + Send + Sync + 'static> Service for EngineFront<A> {
    type Admission = EngineAdmission;
    type Exchange = Exchange;
    type Lease = EngineLease;

    fn admit(&self, peer: SocketAddr) -> Option<EngineAdmission> {
        let held = self.app.admit_owned_connection()?;
        let tracked = self.lane.metrics.track(peer, Protocol::Http1);
        if let Some(tracked) = &tracked {
            tracked.stats().set_handshaking(self.terminates_tls);
        }
        Some(EngineAdmission { held, tracked })
    }

    fn route(&self, request: &Request<'_>, now: Instant) -> Routed<Exchange> {
        assert!(!self.inject_panic, "injected epoll engine panic");
        Exchange::begin(&self.lane, &self.routes, request, now)
            .map_or(Routed::Handoff, Routed::Serve)
    }

    fn select(&self, exchange: &mut Exchange, _now: Instant) -> Selected<EngineLease> {
        exchange.select()
    }

    fn render_request(&self, exchange: &Exchange, request: &Request<'_>, out: &mut Vec<u8>) {
        exchange.render_request(request, out);
    }

    fn render_response(
        &self,
        exchange: &Exchange,
        response: &Response<'_>,
        now: Instant,
        out: &mut Vec<u8>,
    ) {
        exchange.render_response(response, now, out);
    }

    fn judge(&self, exchange: &mut Exchange, answered: &Answered<'_>) -> Recovery {
        exchange.judge(answered)
    }

    fn recover(&self, exchange: &mut Exchange, failure: Failure, _now: Instant) -> Recovery {
        exchange.recover(failure)
    }

    fn replay_safe(&self, exchange: &Exchange) -> bool {
        exchange.replay_safe()
    }

    fn answer(
        &self,
        exchange: &mut Exchange,
        failure: Failure,
        request_is_head: bool,
        out: &mut Vec<u8>,
    ) -> Option<u16> {
        exchange.answer(failure, request_is_head, out)
    }

    fn cancelled(&self, exchange: &Exchange) -> bool {
        exchange.cancelled()
    }

    fn finish(&self, exchange: Exchange, outcome: &Outcome) {
        exchange.finish(&self.lane, outcome);
    }

    fn transferred(&self, admission: &EngineAdmission, transfer: Transfer, now: Instant) {
        admission.transferred(transfer, now);
    }

    fn secured(&self, admission: &EngineAdmission, secured: Secured) {
        admission.secured(secured);
    }

    fn account(&self, admission: &EngineAdmission, exchange: &Exchange, outcome: &Outcome) {
        admission.completed(outcome, exchange.answered_bytes(), outcome.started);
    }

    fn handoff(&self, handoff: Handoff<EngineAdmission>) {
        drop(self.handoffs.send(handoff));
    }
}
