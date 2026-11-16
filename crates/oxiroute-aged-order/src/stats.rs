use std::time::Duration;

/// Counters and gauges of one aged queue (or of all of them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tasks: usize,
    pub ready: usize,
    pub polls: u64,
    pub aged_polls: u64,
    pub oldest_ready: Duration,
    pub age: Duration,
}

impl Stats {
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        Self {
            tasks: self.tasks + other.tasks,
            ready: self.ready + other.ready,
            polls: self.polls + other.polls,
            aged_polls: self.aged_polls + other.aged_polls,
            oldest_ready: self.oldest_ready.max(other.oldest_ready),
            age: self.age.max(other.age),
        }
    }
}
