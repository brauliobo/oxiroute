use std::{
    collections::{BTreeSet, HashMap},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use super::record::RegistrationKey;

/// Monotonic time source, replaceable in tests.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock that only moves when told to.
pub struct ManualClock {
    base: Instant,
    offset_ms: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            offset_ms: AtomicU64::new(0),
        }
    }

    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).expect("test duration fits");
        self.offset_ms.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
    }
}

/// In-memory lease deadlines. Deadlines come from the server clock only and are never persisted.
#[derive(Default)]
pub struct LeaseTable {
    deadlines: HashMap<RegistrationKey, Instant>,
    order: BTreeSet<(Instant, RegistrationKey)>,
}

impl LeaseTable {
    /// Grants or renews a lease that lasts `ttl` from `now`.
    pub fn grant(&mut self, key: RegistrationKey, now: Instant, ttl: Duration) {
        let deadline = now + ttl;
        if let Some(previous) = self.deadlines.insert(key.clone(), deadline) {
            self.order.remove(&(previous, key.clone()));
        }
        self.order.insert((deadline, key));
    }

    pub fn remove(&mut self, key: &RegistrationKey) {
        if let Some(deadline) = self.deadlines.remove(key) {
            self.order.remove(&(deadline, key.clone()));
        }
    }

    pub fn remaining(&self, key: &RegistrationKey, now: Instant) -> Option<Duration> {
        self.deadlines
            .get(key)
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.order.first().map(|(deadline, _)| *deadline)
    }

    /// Removes and returns every lease whose deadline has passed, soonest first.
    pub fn take_expired(&mut self, now: Instant) -> Vec<RegistrationKey> {
        let mut expired = Vec::new();
        while let Some((deadline, _)) = self.order.first() {
            if *deadline > now {
                break;
            }
            let (_, key) = self.order.pop_first().expect("first entry exists");
            self.deadlines.remove(&key);
            expired.push(key);
        }
        expired
    }

    pub fn len(&self) -> usize {
        self.deadlines.len()
    }
}

#[cfg(test)]
mod tests;
