use std::{net::SocketAddr, os::fd::OwnedFd, rc::Rc, time::Duration};

use rustix::event::epoll::EventFlags;

use super::{
    client::{Client, Leased, Stage, Wait},
    clock::Moment,
    config::Trigger,
    link::Link,
    reactor::{DRAINING, Reactor, Step},
    service::{Failure, Service},
    slab::{Body, Token},
    sys,
    upstream::Phase,
};

const ACCEPT_BURST: usize = 128;

impl<S: Service> Reactor<S> {
    pub(crate) fn accept(&mut self, listener: Token, index: usize) {
        for _ in 0..ACCEPT_BURST {
            let Some(Body::Listener { fd, .. }) = self.slab.body(listener) else {
                return;
            };
            match sys::accept(fd) {
                Ok(Some((fd, peer))) => self.admit(fd, peer, index),
                Ok(None) | Err(_) => return,
            }
        }
    }

    fn admit(&mut self, fd: OwnedFd, peer: Option<SocketAddr>, index: usize) {
        let open = self.drain_deadline.is_none() && self.live < self.config.max_clients;
        let Some(peer) = peer.filter(|_| open) else {
            return;
        };
        let (service, timeouts) = (Rc::clone(&self.services[index].0), self.services[index].1);
        let Some(admission) = service.admit(peer) else {
            return;
        };
        let client = Box::new(Client {
            link: Link::new(fd),
            service,
            timeouts,
            listener: index,
            peer,
            admission,
            served: 0,
            stage: Stage::Idle,
            deadline: None,
            buffers: Leased::none(),
            req_len: 0,
            resp_len: 0,
            flow: None,
        });
        let Some(token) = self.slab.insert(Body::Client(client)) else {
            return;
        };
        let Some(Body::Client(client)) = self.slab.body(token) else {
            return;
        };
        let flags = sys::interest_flags(self.config.trigger, true, true);
        if sys::watch(&self.epoll, &client.link.fd, token.to_u64(), flags).is_err() {
            return self.drop_unwatched(token);
        }
        self.live += 1;
        let first = timeouts.first_request;
        self.arm_client(token, first);
    }

    fn drop_unwatched(&mut self, token: Token) {
        if let Body::Client(mut client) = self.slab.remove(token) {
            self.release_buffers(&mut client);
        }
    }

    fn arm_client(&mut self, token: Token, limit: Option<Duration>) {
        let Some(mut client) = self.slab.take_client(token) else {
            return;
        };
        let deadline = limit.map(|limit| self.moment.after(limit));
        self.set_deadline(token, deadline, &mut client.deadline);
        self.slab.put_client(token, client);
    }

    pub(crate) fn set_deadline(
        &mut self,
        token: Token,
        deadline: Option<Moment>,
        slot: &mut Option<Moment>,
    ) {
        *slot = deadline;
        if let Some(deadline) = deadline {
            self.wheel.ensure(token.index, deadline);
        }
    }

    pub(crate) fn lease_buffers(&mut self, client: &mut Client<S>) {
        client.buffers.hold(self.buffers.lease(&self.config));
    }

    pub(crate) fn release_buffers(&mut self, client: &mut Client<S>) {
        if let Some(buffers) = client.buffers.release() {
            self.buffers.give_back(buffers);
        }
    }

    pub(crate) fn client_event(&mut self, token: Token, flags: Option<EventFlags>) {
        let Some(mut client) = self.slab.take_client(token) else {
            return;
        };
        if let Some(flags) = flags {
            client.link.ready.apply(flags);
        }
        self.resume(token, client, Step::Again);
    }

    pub(crate) fn resume(&mut self, token: Token, mut client: Box<Client<S>>, step: Step) {
        let step = if step == Step::Again {
            self.drive(token, &mut client)
        } else {
            step
        };
        match step {
            Step::Again | Step::Wait => {
                if self.config.trigger == Trigger::Level {
                    self.sync_level(token, &mut client);
                }
                self.slab.put_client(token, client);
            }
            Step::Close => self.close_client(token, client),
            Step::Handoff => self.handoff(token, *client),
        }
    }

    pub(crate) fn upstream_event(&mut self, token: Token, flags: EventFlags) {
        let Some(upstream) = self.slab.upstream_mut(token) else {
            return;
        };
        upstream.link.ready.apply(flags);
        let (client, unsolicited) = (
            upstream.client,
            upstream.phase == Phase::Idle && upstream.link.unsolicited(),
        );
        match client {
            Some(client) => self.client_event(client, None),
            None if unsolicited => self.close_upstream(token),
            None => {}
        }
    }

    pub(crate) fn expire(&mut self, token: Token) {
        let deadline = match self.slab.body(token) {
            Some(Body::Client(client)) => client.deadline,
            Some(Body::Upstream(upstream)) => upstream.deadline,
            _ => return,
        };
        match deadline {
            Some(deadline) if deadline > self.moment => self.wheel.schedule(token.index, deadline),
            Some(_) => self.expired_deadline(token),
            None => {}
        }
    }

    fn expired_deadline(&mut self, token: Token) {
        if self.slab.upstream(token).is_some() {
            return self.close_upstream(token);
        }
        let Some(mut client) = self.slab.take_client(token) else {
            return;
        };
        let step = self.timed_out(token, &mut client);
        self.resume(token, client, step);
    }

    fn timed_out(&mut self, token: Token, client: &mut Client<S>) -> Step {
        if client.stage != Stage::Serving {
            return Step::Close;
        }
        let flow = client.flow();
        let failure = if flow.total.is_some_and(|total| total <= self.moment) {
            Failure::TotalTimeout
        } else {
            flow.wait.failure()
        };
        self.fail(token, client, failure)
    }

    pub(crate) fn close_client(&mut self, token: Token, mut client: Box<Client<S>>) {
        if client.flow.is_some() {
            let failure = client.flow().failure.unwrap_or(Failure::ClientGone);
            self.end_exchange(&mut client, Some(failure));
        }
        self.wheel.cancel(token.index);
        self.slab.remove(token);
        self.live -= 1;
        self.release_buffers(&mut client);
    }

    pub(crate) fn close_upstream(&mut self, token: Token) {
        let Some(upstream) = self.slab.upstream(token) else {
            return;
        };
        let (key, idle) = (upstream.key, upstream.phase == Phase::Idle);
        if idle {
            self.pool.remove(key, token);
        }
        self.wheel.cancel(token.index);
        self.slab.remove(token);
    }

    pub(crate) fn begin_drain(&mut self) {
        if self.drain_deadline.is_some() {
            return;
        }
        let _ = self.set_armed(false);
        let draining = self.control.state() == DRAINING;
        self.drain_deadline = Some(if draining {
            self.now + self.config.drain
        } else {
            self.now
        });
        for index in 0..self.slab.len() {
            self.close_if_unserved(index);
        }
    }

    fn close_if_unserved(&mut self, index: usize) {
        let Some(token) = u32::try_from(index)
            .ok()
            .and_then(|index| self.slab.live(index))
        else {
            return;
        };
        match self.slab.body(token) {
            Some(Body::Listener { .. }) => {
                self.slab.remove(token);
            }
            Some(Body::Client(client)) if client.stage == Stage::Idle => {
                if let Some(client) = self.slab.take_client(token) {
                    self.close_client(token, client);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn abort_all(&mut self) {
        self.begin_drain();
        for index in 0..self.slab.len() {
            let Some(token) = u32::try_from(index)
                .ok()
                .and_then(|index| self.slab.live(index))
            else {
                continue;
            };
            if let Some(mut client) = self.slab.take_client(token) {
                if client.flow.is_some() {
                    client.flow().failure = Some(Failure::Shutdown);
                }
                self.close_client(token, client);
            }
        }
    }
}

impl<S: Service> Reactor<S> {
    fn sync_level(&mut self, token: Token, client: &mut Client<S>) {
        let reading = match client.stage {
            Stage::Idle | Stage::Head | Stage::Lingering => true,
            Stage::Serving => client
                .flow
                .as_ref()
                .is_some_and(|flow| flow.wait == Wait::ClientRead),
            Stage::Answering => false,
        };
        let write = !client.link.ready.write;
        rewatch(
            &self.epoll,
            self.config.trigger,
            token,
            &mut client.link,
            (reading, write),
        );
        let Some(flow) = client.flow.as_ref() else {
            return;
        };
        let reading = flow.wait != Wait::ClientWrite;
        let Some(upstream) = flow.upstream else {
            return;
        };
        if let Some(origin) = self.slab.upstream_mut(upstream) {
            let write = !origin.link.ready.write;
            rewatch(
                &self.epoll,
                self.config.trigger,
                upstream,
                &mut origin.link,
                (reading, write),
            );
        }
    }
}

fn rewatch(
    epoll: &OwnedFd,
    trigger: Trigger,
    token: Token,
    link: &mut Link,
    interest: (bool, bool),
) {
    if link.interest != interest {
        link.interest = interest;
        let flags = sys::interest_flags(trigger, interest.0, interest.1);
        let _ = sys::rewatch(epoll, &link.fd, token.to_u64(), flags);
    }
}
