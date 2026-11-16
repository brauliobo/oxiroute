use std::{net::{SocketAddr, TcpStream}, os::fd::OwnedFd, rc::Rc, time::Duration};

use rustix::event::epoll::EventFlags;

use super::{
    client::{Client, Leased, Stage, Wait},
    clock::Moment,
    config::Trigger,
    reactor::{DRAINING, Reactor, Step},
    schedule::Arrival,
    service::{Failure, Handoff, Service},
    slab::{Body, Token},
    sys::{self, Accepted},
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
                Accepted::Connection(fd, peer) => self.admit(fd, peer, index),
                Accepted::Exhausted => self.spare.shed(fd),
                Accepted::Empty | Accepted::Failed => return,
            }
        }
    }

    pub(crate) fn accepted(&mut self, listener: Token, fd: OwnedFd, peer: Option<SocketAddr>) {
        if let Some(Body::Listener { index, .. }) = self.slab.body(listener) {
            let index = *index;
            self.admit(fd, peer, index);
        }
    }

    fn admit(&mut self, fd: OwnedFd, peer: Option<SocketAddr>, index: usize) {
        let Some(peer) = peer.filter(|_| self.drain_deadline.is_none()) else {
            return;
        };
        let Some(admission) = self.services[index].0.admit(peer) else {
            return;
        };
        let wait = self.services[index].1.first_request;
        self.place(Arrival {
            fd,
            unread: Vec::new(),
            peer,
            listener: index,
            admission,
            wait,
        });
    }

    pub(crate) fn install(&mut self, arrival: Arrival<S::Admission>) {
        if self.drain_deadline.is_some() {
            return self.left();
        }
        if arrival.unread.len() > self.config.request_buffer {
            return self.surrender(arrival);
        }
        let Arrival {
            fd,
            unread,
            peer,
            listener,
            admission,
            wait,
        } = arrival;
        let token = self.slab.insert(Body::Busy);
        let Ok(link) = self.driver.open(fd, token) else {
            self.slab.remove(token);
            return self.left();
        };
        let (service, timeouts) = (
            Rc::clone(&self.services[listener].0),
            self.services[listener].1,
        );
        let client = Box::new(Client {
            link,
            service,
            timeouts,
            listener,
            peer,
            admission,
            stage: Stage::Idle,
            deadline: None,
            buffers: Leased::none(),
            req_len: 0,
            resp_len: 0,
            flow: None,
        });
        self.slab.put_client(token, client);
        self.live += 1;
        self.arm_client(token, wait);
        self.replay(token, &unread);
    }

    fn surrender(&mut self, arrival: Arrival<S::Admission>) {
        let Arrival {
            fd,
            unread,
            peer,
            listener,
            admission,
            ..
        } = arrival;
        self.left();
        self.services[listener].0.handoff(Handoff {
            stream: TcpStream::from(fd),
            prefix: unread,
            peer,
            listener,
            admission,
        });
    }

    fn replay(&mut self, token: Token, unread: &[u8]) {
        if unread.is_empty() {
            return;
        }
        let Some(mut client) = self.slab.take_client(token) else {
            return;
        };
        self.lease_buffers(&mut client);
        client.buffers.req[..unread.len()].copy_from_slice(unread);
        client.req_len = unread.len();
        client.stage = Stage::Head;
        let deadline = client.timeouts.request.map(|limit| self.moment.after(limit));
        self.set_deadline(token, deadline, &mut client.deadline);
        self.resume(token, client, Step::Again);
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
                let Some(mut client) = self.relocated(token, client) else {
                    return;
                };
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
            upstream.phase == Phase::Idle
                && (upstream.link.ready.read || upstream.link.ready.hangup),
        );
        match client {
            Some(client) => self.client_ready(client, EventFlags::empty()),
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
        self.left();
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
        for position in 0..self.slab.tokens() {
            self.close_if_unserved(position);
        }
    }

    fn close_if_unserved(&mut self, position: usize) {
        let Some(token) = self.slab.token_at(position) else {
            return;
        };
        match self.slab.body(token) {
            Some(Body::Listener { .. }) => {
                self.slab.remove(token);
                self.driver.remove_listener(token);
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
        for position in 0..self.slab.tokens() {
            let Some(token) = self.slab.token_at(position) else {
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
    pub(crate) fn cancel_requested(&mut self) {
        for position in 0..self.slab.tokens() {
            let Some(token) = self.slab.token_at(position) else {
                continue;
            };
            if matches!(self.slab.body(token), Some(Body::Client(client)) if client.cancelled()) {
                self.cancel(token);
            }
        }
    }

    fn cancel(&mut self, token: Token) {
        let Some(mut client) = self.slab.take_client(token) else {
            return;
        };
        let step = self.fail(token, &mut client, Failure::Cancelled);
        self.resume(token, client, step);
    }

    fn sync_level(&mut self, token: Token, client: &mut Client<S>) {
        let Some(epoll) = self.driver.level() else {
            return;
        };
        let reading = match client.stage {
            Stage::Idle | Stage::Head | Stage::Lingering => true,
            Stage::Serving => client
                .flow
                .as_ref()
                .is_some_and(|flow| flow.wait == Wait::ClientRead),
            Stage::Answering => false,
        };
        let write = !client.link.ready.write;
        epoll.rewatch(token, &mut client.link, (reading, write));
        let Some(flow) = client.flow.as_ref() else {
            return;
        };
        let reading = flow.wait != Wait::ClientWrite;
        let Some(upstream) = flow.upstream else {
            return;
        };
        if let Some(origin) = self.slab.upstream_mut(upstream) {
            let write = !origin.link.ready.write;
            epoll.rewatch(upstream, &mut origin.link, (reading, write));
        }
    }
}
