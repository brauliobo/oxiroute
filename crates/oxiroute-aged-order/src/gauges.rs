use std::{
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{AgedQueue, Stats};

static REGISTRY: Mutex<Vec<Weak<Gauges>>> = Mutex::new(Vec::new());
static EPOCH: OnceLock<Instant> = OnceLock::new();

const NO_READY: u64 = 0;

fn registry() -> MutexGuard<'static, Vec<Weak<Gauges>>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

fn since_epoch(at: Instant) -> u64 {
    let nanos = at.saturating_duration_since(*EPOCH.get_or_init(Instant::now));
    u64::try_from(nanos.as_nanos()).unwrap_or(u64::MAX - 1) + 1
}

/// The figures of one thread-owned queue, published for readers on other threads.
#[derive(Default)]
pub struct Gauges {
    tasks: AtomicUsize,
    ready: AtomicUsize,
    polls: AtomicU64,
    aged_polls: AtomicU64,
    oldest_ready: AtomicU64,
    age_ns: AtomicU64,
}

impl Gauges {
    /// A set of gauges that [`process_stats`] counts for as long as it lives.
    #[must_use]
    pub fn register() -> Arc<Self> {
        let gauges = Arc::new(Self::default());
        let mut live = registry();
        live.retain(|gauges| gauges.strong_count() > 0);
        live.push(Arc::downgrade(&gauges));
        gauges
    }

    pub fn publish<T>(&self, queue: &AgedQueue<T>, tasks: usize) {
        let (polls, aged_polls, age) = queue.counters();
        self.tasks.store(tasks, Ordering::Relaxed);
        self.ready.store(queue.len(), Ordering::Relaxed);
        self.polls.store(polls, Ordering::Relaxed);
        self.aged_polls.store(aged_polls, Ordering::Relaxed);
        self.oldest_ready.store(
            queue.oldest().map_or(NO_READY, since_epoch),
            Ordering::Relaxed,
        );
        self.age_ns.store(
            u64::try_from(age.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn stats(&self) -> Stats {
        let oldest = self.oldest_ready.load(Ordering::Relaxed);
        Stats {
            tasks: self.tasks.load(Ordering::Relaxed),
            ready: self.ready.load(Ordering::Relaxed),
            polls: self.polls.load(Ordering::Relaxed),
            aged_polls: self.aged_polls.load(Ordering::Relaxed),
            oldest_ready: if oldest == NO_READY {
                Duration::ZERO
            } else {
                Duration::from_nanos(since_epoch(Instant::now()).saturating_sub(oldest))
            },
            age: Duration::from_nanos(self.age_ns.load(Ordering::Relaxed)),
        }
    }
}

/// The sum of the figures of every live set of gauges (`oldest_ready` and `age` are the largest).
#[must_use]
pub fn process_stats() -> Stats {
    registry()
        .iter()
        .filter_map(Weak::upgrade)
        .map(|gauges| gauges.stats())
        .fold(Stats::default(), Stats::combine)
}
