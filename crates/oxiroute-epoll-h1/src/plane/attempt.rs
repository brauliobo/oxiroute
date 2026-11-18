use std::{rc::Rc, time::Duration};

use super::{
    client::{Buffers, Client, Down, Flow, Stage, Wait, Window},
    clock::Moment,
    reactor::{Reactor, Step},
    service::{Failure, Lease as _, Recovery, Request, Reuse, Selected, Service, Target},
    slab::{Body, Token},
    upstream::{Phase, Upstream},
};

impl<S: Service> Reactor<S> {
    pub(crate) fn pump(&mut self, token: Token, client: &mut Client<S>) -> Step {
        if client.link.ready.hangup {
            return self.fail(token, client, Failure::ClientGone);
        }
        let Some(upstream) = client.flow().upstream else {
            return self.start_attempt(token, client);
        };
        if let Some(step) = self.settle_connect(token, client, upstream) {
            return step;
        }
        match self.move_data(token, client, upstream) {
            Ok(super::relay::Moved::Progress) => Step::Again,
            Ok(super::relay::Moved::Blocked) => Step::Wait,
            Ok(super::relay::Moved::Complete) => self.complete(token, client),
            Err(failure) => self.fail(token, client, failure),
        }
    }

    fn start_attempt(&mut self, token: Token, client: &mut Client<S>) -> Step {
        let service = Rc::clone(&client.service);
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        flow.attempts += 1;
        let exchange = flow.exchange.as_mut().expect("an exchange is served once");
        match service.select(exchange, self.now) {
            Selected::Server(target) => self.attach(token, client, target),
            Selected::Handoff if !flow.contacted => Step::Handoff,
            Selected::Handoff => self.fail(token, client, Failure::NoServer),
            Selected::Refused(failure) => self.fail(token, client, failure),
        }
    }

    fn attach(&mut self, token: Token, client: &mut Client<S>, target: Target<S::Lease>) -> Step {
        let Target {
            address,
            key,
            reuse,
            lease,
            timeouts,
        } = target;
        client.resp_len = 0;
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        let total = timeouts.deadline.map(|at| self.clock.moment(at));
        flow.begin_attempt(self.moment, total, key, reuse, timeouts);
        let pooled = matches!(reuse, Reuse::Pooled { .. });
        if let Some(upstream) = pooled.then(|| self.checkout(key)).flatten() {
            drop(lease);
            self.bind_idle(token, client, upstream);
            return Step::Again;
        }
        self.connect(token, client, address, lease)
    }

    fn checkout(&mut self, key: u64) -> Option<Token> {
        while let Some(token) = self.pool.take(key) {
            let upstream = self.slab.upstream_mut(token)?;
            if upstream.link.ready.closing || upstream.link.has_input() {
                self.close_upstream_detached(token);
                continue;
            }
            upstream.lease.idle(false);
            return Some(token);
        }
        None
    }

    fn bind_idle(&mut self, token: Token, client: &mut Client<S>, upstream: Token) {
        self.wheel.cancel(upstream.index);
        if let Some(idle) = self.slab.upstream_mut(upstream) {
            idle.phase = Phase::Busy;
            idle.client = Some(token);
            idle.deadline = None;
        }
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        flow.upstream = Some(upstream);
        flow.reused = true;
        flow.connected_at = Some(self.moment);
        self.start_request(token, client);
    }

    fn connect(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        address: std::net::SocketAddr,
        lease: S::Lease,
    ) -> Step {
        if !lease.connecting() {
            return self.fail(token, client, Failure::NoCapacity);
        }
        let upstream_token = self.slab.insert(Body::Busy);
        let Ok((link, established)) = self.driver.dial(upstream_token, address) else {
            self.slab.remove(upstream_token);
            return self.fail(token, client, Failure::Connect);
        };
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        let phase = if established {
            Phase::Busy
        } else {
            Phase::Connecting
        };
        let idle = match flow.reuse {
            Reuse::Pooled { idle } => Some(idle),
            Reuse::Never => None,
        };
        let upstream = Upstream {
            link,
            address,
            key: flow.key,
            phase,
            client: Some(token),
            deadline: None,
            lease,
            idle,
        };
        self.slab.fill(upstream_token, Body::Upstream(upstream));
        flow.upstream = Some(upstream_token);
        if established {
            flow.connected_at = Some(self.moment);
            self.start_request(token, client);
        } else {
            let limit = flow.timeouts.map(|timeouts| timeouts.connect);
            self.wait_for(token, client, Wait::Connect, limit);
        }
        Step::Again
    }

    fn settle_connect(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        upstream: Token,
    ) -> Option<Step> {
        let connecting = self.slab.upstream_mut(upstream)?;
        if connecting.phase != Phase::Connecting {
            return None;
        }
        if !(connecting.link.ready.write || connecting.link.ready.hangup) {
            return Some(Step::Wait);
        }
        if !connecting.link.connect_succeeded() {
            return Some(self.fail(token, client, Failure::Connect));
        }
        connecting.phase = Phase::Busy;
        client.flow().connected_at = Some(self.moment);
        self.start_request(token, client);
        None
    }

    fn start_request(&mut self, token: Token, client: &mut Client<S>) {
        let service = Rc::clone(&client.service);
        let Client {
            buffers,
            flow,
            link,
            peer,
            req_len,
            ..
        } = client;
        let flow = flow.as_mut().expect("a serving client has a flow");
        let buffers: &mut Buffers = buffers;
        buffers.up_head.clear();
        let bytes = &buffers.req[..*req_len];
        let secure = link.encrypted();
        let waker = (&self.wake, flow.replayable);
        let request = Request::new(bytes, &buffers.head, *peer, secure, waker);
        let exchange = flow.exchange.as_ref().expect("an exchange is served once");
        service.render_request(exchange, &request, &mut buffers.up_head);
        let limit = flow.timeouts.map(|timeouts| timeouts.write);
        self.wait_for(token, client, Wait::UpstreamWrite, limit);
    }

    pub(crate) fn wait_for(
        &mut self,
        token: Token,
        client: &mut Client<S>,
        wait: Wait,
        limit: Option<Duration>,
    ) {
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        flow.wait = wait;
        let step = limit.map(|limit| self.moment.after(limit));
        let deadline = match (step, flow.total) {
            (Some(step), Some(total)) => Some(step.min(total)),
            (step, total) => step.or(total),
        };
        self.set_deadline(token, deadline, &mut client.deadline);
    }

    pub(crate) fn fail(&mut self, token: Token, client: &mut Client<S>, failure: Failure) -> Step {
        let flow = client.flow();
        let upstream = flow.upstream.take();
        if let Some(stale) = upstream.filter(|_| flow.closed_while_idle(failure)) {
            return self.redial(token, client, stale);
        }
        self.release_upstream(upstream, false);
        if self.retries(client, failure) {
            client.flow().down = Down::Awaiting;
            return Step::Again;
        }
        self.give_up(token, client, failure)
    }

    fn redial(&mut self, token: Token, client: &mut Client<S>, stale: Token) -> Step {
        self.wheel.cancel(stale.index);
        let Body::Upstream(Upstream { address, lease, .. }) = self.slab.remove(stale) else {
            unreachable!("a reused attempt holds an upstream");
        };
        client.resp_len = 0;
        client.flow().restart(self.moment);
        self.connect(token, client, address, lease)
    }

    fn retries(&mut self, client: &mut Client<S>, failure: Failure) -> bool {
        let service = Rc::clone(&client.service);
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        let client_side = matches!(
            failure,
            Failure::ClientGone
                | Failure::ClientTimeout
                | Failure::Shutdown
                | Failure::TotalTimeout
        );
        if client_side || !flow.replayable() || flow.attempts >= self.config.max_attempts {
            return false;
        }
        let exchange = flow.exchange.as_mut().expect("an exchange is served once");
        failure == Failure::UnexpectedStatus
            || service.recover(exchange, failure, self.now) == Recovery::Retry
    }

    fn give_up(&mut self, token: Token, client: &mut Client<S>, failure: Failure) -> Step {
        let service = Rc::clone(&client.service);
        let flow = client.flow.as_mut().expect("a serving client has a flow");
        flow.failure = Some(failure);
        flow.keepalive = false;
        let client_side = matches!(
            failure,
            Failure::ClientGone | Failure::ClientTimeout | Failure::Shutdown
        );
        if client_side || flow.status.is_some() || flow.origin.torn() {
            return Step::Close;
        }
        let down_head = &mut client.buffers.down_head;
        down_head.clear();
        let exchange = flow.exchange.as_mut().expect("an exchange is served once");
        let Some(status) = service.answer(exchange, failure, flow.is_head, down_head) else {
            return Step::Close;
        };
        flow.status = Some(status);
        flow.head_pos = 0;
        client.stage = Stage::Answering;
        let limit = client.timeouts.write;
        self.set_deadline(
            token,
            limit.map(|limit| self.moment.after(limit)),
            &mut client.deadline,
        );
        Step::Again
    }
}

impl<S: Service> Flow<S> {
    fn replayable(&self) -> bool {
        self.replayable
            && self.status.is_none()
            && matches!(self.down, Down::Awaiting)
            && !self.origin.forwarded()
    }

    fn closed_while_idle(&self, failure: Failure) -> bool {
        self.reused
            && self.first_byte_at.is_none()
            && matches!(failure, Failure::UpstreamClosed | Failure::UpstreamWrite)
            && self.replayable()
    }

    fn begin_attempt(
        &mut self,
        now: Moment,
        total: Option<Moment>,
        key: u64,
        reuse: Reuse,
        timeouts: super::service::AttemptTimeouts,
    ) {
        self.contacted = true;
        self.total = total;
        self.key = key;
        self.reuse = reuse;
        self.timeouts = Some(timeouts);
        self.restart(now);
    }

    fn restart(&mut self, now: Moment) {
        self.origin.restart();
        self.attempt_started = now;
        self.connected_at = None;
        self.first_byte_at = None;
        self.reused = false;
        self.head_pos = 0;
        self.window = Window::new(self.head_len, self.request_end);
        self.upload_done = false;
        self.upload_failed = false;
        self.down = Down::Awaiting;
    }
}
