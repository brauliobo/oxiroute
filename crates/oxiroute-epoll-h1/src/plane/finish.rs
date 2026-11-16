use std::rc::Rc;

use super::{
    client::{Client, Down, Stage},
    reactor::{Reactor, Step},
    service::{Failure, Lease as _, Outcome, Service},
    slab::Token,
    sys,
    upstream::Phase,
};

impl<S: Service> Reactor<S> {
    pub(crate) fn complete(&mut self, token: Token, client: &mut Client<S>) -> Step {
        let flow = client.flow();
        let reusable = flow.upload_done && matches!(flow.down, Down::Finished { reusable: true });
        let (upstream, failure) = (flow.upstream.take(), flow.failure);
        let (keepalive, unread, request_end) = (
            flow.keepalive && failure.is_none() && self.drain_deadline.is_none(),
            flow.body_unread > 0,
            flow.request_end,
        );
        self.release_upstream(upstream, reusable);
        self.end_exchange(client, failure);
        if keepalive {
            self.next_request(token, client, request_end)
        } else {
            self.closing(token, client, unread)
        }
    }

    fn next_request(&mut self, token: Token, client: &mut Client<S>, request_end: usize) -> Step {
        let carry = client.req_len - request_end;
        client
            .buffers
            .req
            .copy_within(request_end..client.req_len, 0);
        client.req_len = carry;
        client.resp_len = 0;
        let (stage, limit) = if carry > 0 {
            (Stage::Head, client.timeouts.request)
        } else {
            (Stage::Idle, client.timeouts.keepalive)
        };
        client.stage = stage;
        if stage == Stage::Idle {
            self.release_buffers(client);
        }
        self.set_deadline(
            token,
            limit.map(|limit| self.moment.after(limit)),
            &mut client.deadline,
        );
        Step::Again
    }

    fn closing(&mut self, token: Token, client: &mut Client<S>, unread: bool) -> Step {
        if !unread {
            return Step::Close;
        }
        sys::shutdown_write(&client.link.fd);
        client.stage = Stage::Lingering;
        let deadline = Some(self.moment.after(self.config.linger));
        self.set_deadline(token, deadline, &mut client.deadline);
        Step::Again
    }

    pub(crate) fn end_exchange(&mut self, client: &mut Client<S>, failure: Option<Failure>) {
        let Some(mut flow) = client.flow.take() else {
            return;
        };
        self.release_upstream(flow.upstream.take(), false);
        let Some(exchange) = flow.exchange.take() else {
            return;
        };
        let outcome = Outcome {
            status: flow.status,
            failure,
            attempts: flow.attempts,
            received: flow.received,
            sent: flow.sent,
            started: flow.started,
            ended: self.now,
            connect: flow.connected_at.map(|at| at.since(flow.attempt_started)),
            header: flow.first_byte_at.map(|at| at.since(flow.attempt_started)),
            keepalive: flow.keepalive,
            splice: flow.splice,
        };
        Rc::clone(&client.service).finish(exchange, &outcome);
    }

    pub(crate) fn release_upstream(&mut self, token: Option<Token>, reusable: bool) {
        let Some(token) = token else { return };
        let Some(upstream) = self.slab.upstream_mut(token) else {
            return;
        };
        let ready = upstream.link.ready;
        let idle = upstream.idle.filter(|_| {
            reusable && upstream.phase == Phase::Busy && !ready.closing && !ready.hangup
        });
        if let Some(idle) = idle
            && self.pool.put(upstream.key, token)
        {
            upstream.phase = Phase::Idle;
            upstream.lease.idle(true);
            upstream.client = None;
            let deadline = self.moment.after(idle);
            upstream.deadline = Some(deadline);
            self.wheel.ensure(token.index, deadline);
            return;
        }
        self.close_upstream_detached(token);
    }

    pub(crate) fn close_upstream_detached(&mut self, token: Token) {
        self.wheel.cancel(token.index);
        self.slab.remove(token);
    }
}
