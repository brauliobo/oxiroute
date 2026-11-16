use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::{Age, Stats, governor::Governor};

pub struct AgedQueue<T> {
    policy: Age,
    ready: VecDeque<(T, Instant)>,
    clock: Option<Instant>,
    age: Duration,
    governor: Governor,
    polls: u64,
    aged_polls: u64,
}

impl<T> AgedQueue<T> {
    #[must_use]
    pub fn new(policy: Age) -> Self {
        Self {
            policy,
            ready: VecDeque::new(),
            clock: None,
            age: policy.initial(),
            governor: Governor::default(),
            polls: 0,
            aged_polls: 0,
        }
    }

    /// Starts a batch at `now`: ready items are stamped with it, and an adaptive age follows the items ready now
    /// plus the `submitted` ones the caller is about to push.
    pub fn begin_batch(&mut self, now: Instant, submitted: usize) {
        self.clock = Some(now);
        if let Age::Adaptive(multiple) = self.policy {
            let ready = self.ready.len() + submitted;
            self.age = self.governor.age(multiple, (now, self.polls), ready);
        }
    }

    pub fn push(&mut self, item: T) {
        let clock = match self.clock {
            Some(clock) if !self.ready.is_empty() => clock,
            _ => *self.clock.insert(Instant::now()),
        };
        self.ready.push_back((item, clock));
    }

    /// The newest ready item, or the oldest when it has waited for the age as of `now`.
    pub fn pop(&mut self, now: Instant) -> Option<T> {
        let aged = self
            .ready
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= self.age);
        let Some((item, _)) = (if aged {
            self.ready.pop_front()
        } else {
            self.ready.pop_back()
        }) else {
            self.governor.rest();
            return None;
        };
        self.polls += 1;
        self.aged_polls += u64::from(aged);
        Some(item)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ready.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    pub fn drain(&mut self) -> VecDeque<(T, Instant)> {
        std::mem::take(&mut self.ready)
    }

    #[must_use]
    pub fn oldest(&self) -> Option<Instant> {
        self.ready.front().map(|(_, at)| *at)
    }

    #[must_use]
    pub fn stats(&self, tasks: usize) -> Stats {
        Stats {
            tasks,
            ready: self.ready.len(),
            polls: self.polls,
            aged_polls: self.aged_polls,
            oldest_ready: self.oldest().map_or(Duration::ZERO, |at| at.elapsed()),
            age: self.age,
        }
    }

    pub(crate) fn counters(&self) -> (u64, u64, Duration) {
        (self.polls, self.aged_polls, self.age)
    }
}

#[cfg(test)]
mod tests;
