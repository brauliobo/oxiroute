use std::{sync::Arc, time::Instant};

use oxiroute_aged_order::{Age, AgedQueue, Gauges};
use rustix::event::epoll::EventFlags;

use crate::plane::slab::Token;

#[derive(Clone, Copy)]
struct Pending {
    token: Token,
    flags: EventFlags,
    queued: bool,
}

const IDLE: Pending = Pending {
    token: Token {
        index: 0,
        generation: 0,
    },
    flags: EventFlags::empty(),
    queued: false,
};

pub(crate) struct Order {
    queue: AgedQueue<u32>,
    pending: Vec<Pending>,
    gauges: Arc<Gauges>,
}

impl Order {
    pub(crate) fn new(age: Age) -> Self {
        Self {
            queue: AgedQueue::new(age),
            pending: Vec::new(),
            gauges: Gauges::register(),
        }
    }

    pub(crate) fn begin(&mut self, now: Instant, arriving: usize) {
        self.queue.begin_batch(now, arriving);
    }

    pub(crate) fn ready(&mut self, token: Token, flags: EventFlags) {
        let index = token.index as usize;
        if self.pending.len() <= index {
            self.pending.resize(index + 1, IDLE);
        }
        let slot = &mut self.pending[index];
        let queued = slot.queued;
        *slot = Pending {
            token,
            flags: if queued && slot.token == token {
                slot.flags | flags
            } else {
                flags
            },
            queued: true,
        };
        if !queued {
            self.queue.push(token.index);
        }
    }

    pub(crate) fn next(&mut self, now: Instant) -> Option<(Token, EventFlags)> {
        let index = self.queue.pop(now)?;
        let slot = &mut self.pending[index as usize];
        slot.queued = false;
        Some((slot.token, slot.flags))
    }

    pub(crate) fn backlogged(&self) -> bool {
        !self.queue.is_empty()
    }

    pub(crate) fn publish(&self, connections: usize) {
        self.gauges.publish(&self.queue, connections);
    }
}

#[cfg(test)]
mod tests;
