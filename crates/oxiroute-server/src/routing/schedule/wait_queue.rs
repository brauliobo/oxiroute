//! The waiting requests of a pool in service order.
//!
//! Every operation costs O(classes + log waiters): a class is picked by virtual time or priority, the
//! class serves its least served tenant, and a tenant serves its oldest arrival. Without a class
//! schedule everybody shares one class and tenant, so the order is the arrival order and only the
//! arrival index is kept.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

use super::{QueueSchedule, QueueTag};
use crate::routing::PoolWaiter;

#[derive(Debug)]
struct Entry {
    waiter: Arc<PoolWaiter>,
    tag: QueueTag,
    /// Arrival time, kept only where aging reads it.
    since: Option<Instant>,
}

/// One tenant of a class: how many of its requests were served and who of them still waits.
#[derive(Debug, Default)]
struct TenantQueue {
    served: u64,
    waiting: BTreeSet<u64>,
    /// The `(served, oldest arrival)` under which the class orders this tenant while it waits.
    ranked: Option<(u64, u64)>,
}

#[derive(Debug, Default)]
struct ClassQueue {
    /// Virtual time: served requests divided by the class weight.
    time: f64,
    waiting: u64,
    tenants: BTreeMap<u32, TenantQueue>,
    /// Waiting tenants, least served first, then by oldest arrival.
    order: BTreeSet<(u64, u64, u32)>,
    /// Served count of the tenant most recently served.
    tenant_time: u64,
}

impl ClassQueue {
    /// A class or tenant that was idle starts at the current time, so idleness earns no credit.
    fn arrive(&mut self, tenant: u32, seq: u64, virtual_time: f64) {
        if self.waiting == 0 {
            self.time = self.time.max(virtual_time);
        }
        let queue = self.tenants.entry(tenant).or_default();
        if queue.waiting.is_empty() {
            queue.served = queue.served.max(self.tenant_time);
        }
        queue.waiting.insert(seq);
        self.waiting += 1;
        self.rank(tenant);
    }

    fn leave(&mut self, tenant: u32, seq: u64) {
        if let Some(queue) = self.tenants.get_mut(&tenant) {
            queue.waiting.remove(&seq);
        }
        self.waiting -= 1;
        self.rank(tenant);
    }

    /// The tenant used up one turn of its class.
    fn spend_turn(&mut self, tenant: u32, weight: f64) {
        self.time += 1.0 / weight;
        let queue = self.tenants.entry(tenant).or_default();
        self.tenant_time = self.tenant_time.max(queue.served);
        queue.served += 1;
        self.rank(tenant);
    }

    fn rank(&mut self, tenant: u32) {
        let Some(queue) = self.tenants.get_mut(&tenant) else {
            return;
        };
        if let Some((served, oldest)) = queue.ranked.take() {
            self.order.remove(&(served, oldest, tenant));
        }
        if let Some(&oldest) = queue.waiting.first() {
            queue.ranked = Some((queue.served, oldest));
            self.order.insert((queue.served, oldest, tenant));
        }
    }

    fn head(&self) -> Option<u64> {
        self.order.first().map(|&(_, oldest, _)| oldest)
    }
}

/// The waiting requests of a pool in service order.
#[derive(Debug)]
pub(in crate::routing) struct WaitQueue {
    schedule: Option<Arc<QueueSchedule>>,
    next_seq: u64,
    entries: BTreeMap<u64, Entry>,
    classes: Vec<ClassQueue>,
    /// The class time of the latest service.
    virtual_time: f64,
}

impl WaitQueue {
    pub(in crate::routing) fn new(schedule: Option<Arc<QueueSchedule>>) -> Self {
        let classes = schedule
            .as_ref()
            .map_or(1, |schedule| schedule.classes.len());
        Self {
            schedule,
            next_seq: 0,
            entries: BTreeMap::new(),
            classes: (0..classes).map(|_| ClassQueue::default()).collect(),
            virtual_time: 0.0,
        }
    }

    pub(in crate::routing) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Queues a waiter and returns its arrival number, which names it from then on.
    pub(in crate::routing) fn push(&mut self, waiter: Arc<PoolWaiter>, tag: QueueTag) -> u64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        if self.schedule.is_some() {
            self.classes[tag.class].arrive(tag.tenant, seq, self.virtual_time);
        }
        let since = self.aging().map(|_| Instant::now());
        self.entries.insert(seq, Entry { waiter, tag, since });
        seq
    }

    fn aging(&self) -> Option<std::time::Duration> {
        self.schedule
            .as_ref()
            .filter(|schedule| schedule.strict)
            .and_then(|schedule| schedule.aging)
    }

    /// The waiter that may select next.
    pub(in crate::routing) fn head(&self) -> Option<(u64, &Arc<PoolWaiter>)> {
        let seq = self.head_seq()?;
        self.entries.get(&seq).map(|entry| (seq, &entry.waiter))
    }

    fn head_seq(&self) -> Option<u64> {
        if self.schedule.is_none() {
            return self.entries.first_key_value().map(|(&seq, _)| seq);
        }
        if let Some(aging) = self.aging()
            && let Some((&seq, oldest)) = self.entries.first_key_value()
            && oldest.since.is_some_and(|since| since.elapsed() >= aging)
        {
            return Some(seq);
        }
        let class = self.next_class()?;
        self.classes[class].head()
    }

    fn next_class(&self) -> Option<usize> {
        let mut waiting = self
            .classes
            .iter()
            .enumerate()
            .filter(|(_, class)| class.waiting > 0);
        match &self.schedule {
            Some(schedule) if !schedule.strict => waiting
                .min_by(|(a, first), (b, second)| first.time.total_cmp(&second.time).then(a.cmp(b)))
                .map(|(class, _)| class),
            _ => waiting.next().map(|(class, _)| class),
        }
    }

    /// Removes a waiter and reports whether it was the head. A served waiter spends its turn.
    pub(in crate::routing) fn remove(&mut self, seq: u64, served: bool) -> bool {
        let was_head = self.head_seq() == Some(seq);
        let Some(entry) = self.entries.remove(&seq) else {
            return false;
        };
        let QueueTag { class, tenant } = entry.tag;
        if let Some(schedule) = &self.schedule {
            self.classes[class].leave(tenant, seq);
            if served {
                let weight = f64::from(schedule.classes[class].1);
                self.virtual_time = self.virtual_time.max(self.classes[class].time);
                self.classes[class].spend_turn(tenant, weight);
            }
        }
        was_head
    }

    pub(in crate::routing) fn is_head(&self, seq: u64) -> bool {
        self.head_seq() == Some(seq)
    }

    /// Waiting requests per class, in class order.
    pub(in crate::routing) fn depth_by_class(&self) -> Vec<u64> {
        if self.schedule.is_none() {
            return vec![self.entries.len() as u64];
        }
        self.classes.iter().map(|class| class.waiting).collect()
    }
}
