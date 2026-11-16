use std::io;

use oxiroute_uring::Released;

use super::arrival::Arrival;
use crate::plane::{
    client::{Client, Stage},
    reactor::Reactor,
    service::Service,
    slab::{Body, Token},
};

impl<S: Service> Reactor<S> {
    pub(crate) fn watch_inbox(&mut self) -> io::Result<()> {
        let Some(schedule) = &self.schedule else {
            return Ok(());
        };
        let token = self
            .slab
            .insert(Body::Inbox(schedule.placement.inbox_waker()));
        let Some(Body::Inbox(waker)) = self.slab.body(token) else {
            unreachable!("just inserted")
        };
        self.driver.watch_wake(token, waker.fd())
    }

    pub(crate) fn place(&mut self, arrival: Arrival<S::Admission>) {
        if let Some(local) = self.send_unless_local(arrival) {
            self.install(local);
        }
    }

    fn send_unless_local(&self, arrival: Arrival<S::Admission>) -> Option<Arrival<S::Admission>> {
        let Some(schedule) = &self.schedule else {
            return Some(arrival);
        };
        let thread = schedule.placement.choose();
        if thread == schedule.placement.own() {
            return Some(arrival);
        }
        schedule.placement.send(thread, arrival);
        None
    }

    pub(crate) fn arrived(&mut self, token: Token) {
        let Some(schedule) = &mut self.schedule else {
            return;
        };
        schedule.placement.settle_inbox();
        self.driver.wake_consumed(token);
        let mut arrivals = std::mem::take(&mut schedule.arrivals);
        schedule.placement.take(&mut arrivals);
        for arrival in arrivals.drain(..) {
            self.install(arrival);
        }
        if let Some(schedule) = &mut self.schedule {
            schedule.arrivals = arrivals;
        }
    }

    pub(crate) fn left(&self) {
        if let Some(schedule) = &self.schedule {
            schedule.placement.left();
        }
    }

    pub(crate) fn relocated(
        &mut self,
        token: Token,
        client: Box<Client<S>>,
    ) -> Option<Box<Client<S>>> {
        let thread = match &self.schedule {
            Some(schedule)
                if client.stage == Stage::Idle && schedule.placement.wants_relocation() =>
            {
                schedule.placement.relocation()
            }
            _ => None,
        };
        let Some(thread) = thread else {
            return Some(client);
        };
        self.send_away(token, *client, thread);
        None
    }

    fn send_away(&mut self, token: Token, client: Client<S>, thread: usize) {
        let wait = client.deadline.map(|deadline| deadline.since(self.moment));
        self.wheel.cancel(token.index);
        self.slab.remove(token);
        self.live -= 1;
        let Client {
            link,
            peer,
            admission,
            listener,
            ..
        } = client;
        let Ok(Released { fd, unread }) = self.driver.release(link) else {
            return;
        };
        if let Some(schedule) = &self.schedule {
            let arrival = Arrival {
                fd,
                unread,
                peer,
                listener,
                admission,
                wait,
            };
            schedule.placement.send(thread, arrival);
        }
    }
}
