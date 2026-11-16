use std::time::Instant;

use rustix::event::epoll::EventFlags;

use crate::plane::{reactor::Reactor, service::Service, slab::Token};

const BATCH: usize = 32;

impl<S: Service> Reactor<S> {
    pub(crate) fn begin_batch(&mut self, arriving: usize) {
        if let Some(schedule) = &mut self.schedule {
            schedule.order.begin(self.now, arriving);
        }
    }

    pub(crate) fn client_ready(&mut self, token: Token, flags: EventFlags) {
        match &mut self.schedule {
            Some(schedule) => schedule.order.ready(token, flags),
            None => self.client_event(token, Some(flags)),
        }
    }

    pub(crate) fn serve_ready(&mut self) {
        for _ in 0..BATCH {
            let next = self
                .schedule
                .as_mut()
                .and_then(|schedule| schedule.order.next(self.now));
            let Some((token, flags)) = next else { break };
            self.client_event(token, Some(flags));
        }
        if let Some(schedule) = &self.schedule {
            schedule.order.publish(self.live);
        }
    }

    pub(crate) fn backlogged(&self) -> bool {
        self.schedule
            .as_ref()
            .is_some_and(|schedule| schedule.order.backlogged())
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
