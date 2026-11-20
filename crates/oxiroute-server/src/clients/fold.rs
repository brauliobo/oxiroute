use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    key::IpKey,
    stats::{ConnStats, Counts},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Totals {
    pub(super) connections: u64,
    pub(super) counts: Counts,
}

impl Totals {
    pub(super) const fn plus(self, other: Self) -> Self {
        Self {
            connections: self.connections + other.connections,
            counts: Counts {
                requests: self.counts.requests + other.counts.requests,
                errors: self.counts.errors + other.counts.errors,
                bytes_in: self.counts.bytes_in + other.counts.bytes_in,
                bytes_out: self.counts.bytes_out + other.counts.bytes_out,
            },
        }
    }
}

pub(super) struct Folded {
    pub(super) totals: Totals,
    pub(super) first: Instant,
    pub(super) last: Instant,
    pub(super) listeners: Vec<Arc<str>>,
    pub(super) protocols: u16,
}

impl Folded {
    fn new(stats: &ConnStats) -> Self {
        Self {
            totals: Totals::default(),
            first: stats.opened(),
            last: stats.opened(),
            listeners: Vec::new(),
            protocols: 0,
        }
    }

    fn absorb(&mut self, stats: &ConnStats) {
        let closed = Totals {
            connections: 1,
            counts: stats.counts(),
        };
        self.totals = self.totals.plus(closed);
        self.first = self.first.min(stats.opened());
        self.last = self
            .last
            .max(stats.opened() + Duration::from_millis(stats.last_activity_ms()));
        self.protocols |= stats.protocol().bit();
        if !self
            .listeners
            .iter()
            .any(|listener| Arc::ptr_eq(listener, stats.listener()))
        {
            self.listeners.push(Arc::clone(stats.listener()));
        }
    }
}

#[derive(Default)]
pub(super) struct History {
    folded: HashMap<IpKey, Folded>,
    evicted: Totals,
}

impl History {
    pub(super) fn fold(&mut self, stats: &ConnStats, capacity: usize) {
        self.folded
            .entry(stats.key())
            .or_insert_with(|| Folded::new(stats))
            .absorb(stats);
        if self.folded.len() > capacity {
            self.evict(capacity);
        }
    }

    fn evict(&mut self, capacity: usize) {
        let mut by_recency = self
            .folded
            .iter()
            .map(|(key, folded)| (folded.last, *key))
            .collect::<Vec<_>>();
        let evicting = (self.folded.len() - capacity + capacity / 8).min(by_recency.len());
        by_recency.select_nth_unstable_by_key(evicting - 1, |(last, _)| *last);
        for (_, key) in &by_recency[..evicting] {
            if let Some(folded) = self.folded.remove(key) {
                self.evicted = self.evicted.plus(folded.totals);
            }
        }
    }

    pub(super) fn clients(&self) -> impl Iterator<Item = (IpKey, &Folded)> {
        self.folded.iter().map(|(key, folded)| (*key, folded))
    }

    pub(super) const fn evicted(&self) -> Totals {
        self.evicted
    }
}
