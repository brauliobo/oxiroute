use std::time::Instant;

use rustix::event::epoll::EventFlags;

use super::order::Order;
use crate::plane::{reactor::Reactor, service::Service, slab::Token};

const BATCH: usize = 32;

impl<S: Service> Reactor<S> {
    fn order(&mut self) -> Option<&mut Order> {
        self.schedule.as_mut()?.order.as_mut()
    }

    pub(crate) fn begin_batch(&mut self, arriving: usize) {
        let now = self.now;
        if let Some(order) = self.order() {
            order.begin(now, arriving);
        }
    }

    pub(crate) fn client_ready(&mut self, token: Token, flags: EventFlags) {
        match self.order() {
            Some(order) => order.ready(token, flags),
            None => self.client_event(token, Some(flags)),
        }
    }

    pub(crate) fn serve_ready(&mut self) {
        let now = self.now;
        for _ in 0..BATCH {
            let Some((token, flags)) = self.order().and_then(|order| order.next(now)) else {
                break;
            };
            self.client_event(token, Some(flags));
        }
        let live = self.live;
        if let Some(order) = self.order() {
            order.publish(live);
        }
    }

    pub(crate) fn backlogged(&self) -> bool {
        self.schedule
            .as_ref()
            .and_then(|schedule| schedule.order.as_ref())
            .is_some_and(Order::backlogged)
    }

    pub(crate) fn account_busy(&self) {
        let Some(schedule) = self
            .schedule
            .as_ref()
            .filter(|schedule| schedule.placement.scales())
        else {
            return;
        };
        let spent = Instant::now().saturating_duration_since(self.now);
        schedule
            .placement
            .worked(u64::try_from(spent.as_nanos()).unwrap_or(u64::MAX));
    }
}
