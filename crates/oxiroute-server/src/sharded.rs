//! Counters that every worker thread updates on every request.
//!
//! One shared atomic is a cache line the cores pass back and forth, and a request pays for it in
//! proportion to the number of workers. A [`Sharded`] value keeps one copy per slot instead: each
//! thread writes the slot it was given on first use, readers add the slots up. Slots sit on their
//! own cache lines, and threads beyond [`SHARDS`] share slots round robin.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub(crate) const SHARDS: usize = 8;

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

std::thread_local! {
    static SLOT: usize = NEXT_SLOT.fetch_add(1, Ordering::Relaxed) % SHARDS;
}

/// The slot number of the calling thread, below [`SHARDS`].
pub(crate) fn local_index() -> usize {
    SLOT.with(|slot| *slot)
}

/// A value alone on its cache lines; two lines wide so the adjacent-line prefetcher does not
/// pair neighbouring slots.
#[derive(Debug)]
#[repr(align(128))]
pub(crate) struct Padded<T>(pub(crate) T);

#[derive(Debug)]
pub(crate) struct Sharded<T> {
    slots: [Padded<T>; SHARDS],
}

impl<T> Sharded<T> {
    pub(crate) fn new(mut make: impl FnMut() -> T) -> Self {
        Self {
            slots: std::array::from_fn(|_| Padded(make())),
        }
    }

    /// The slot of the calling thread.
    pub(crate) fn local(&self) -> &T {
        &self.slots[local_index()].0
    }

    pub(crate) fn get(&self, index: usize) -> &T {
        &self.slots[index].0
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().map(|slot| &slot.0)
    }
}

/// `N` counters that every worker thread bumps, one copy per slot; a read adds the slots up.
///
/// The total saturates instead of wrapping; a slot would need 2^64 events to overflow.
#[derive(Debug)]
pub(crate) struct ShardedCounters<const N: usize>(Sharded<[AtomicU64; N]>);

impl<const N: usize> ShardedCounters<N> {
    pub(crate) fn new() -> Self {
        Self(Sharded::new(|| std::array::from_fn(|_| AtomicU64::new(0))))
    }

    pub(crate) fn add(&self, index: usize, value: u64) {
        self.0.local()[index].fetch_add(value, Ordering::Relaxed);
    }

    pub(crate) fn get(&self, index: usize) -> u64 {
        self.0.iter().fold(0, |total, slot| {
            total.saturating_add(slot[index].load(Ordering::Relaxed))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_add_up_across_threads() {
        let counter = Sharded::new(|| AtomicU64::new(0));
        std::thread::scope(|scope| {
            for _ in 0..(SHARDS * 2) {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        counter.local().fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        let total: u64 = counter
            .iter()
            .map(|slot| slot.load(Ordering::Relaxed))
            .sum();
        assert_eq!(total, 1000 * SHARDS as u64 * 2);
    }

    #[test]
    fn counters_add_up_across_threads() {
        let counters = ShardedCounters::<3>::new();
        std::thread::scope(|scope| {
            for _ in 0..(SHARDS * 2) {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        counters.add(1, 2);
                    }
                });
            }
        });
        assert_eq!(counters.get(0), 0);
        assert_eq!(counters.get(1), 2000 * SHARDS as u64 * 2);
    }

    #[test]
    fn a_thread_keeps_its_slot() {
        let counter = Sharded::new(|| AtomicU64::new(0));
        std::ptr::eq(counter.local(), counter.local())
            .then_some(())
            .expect("same slot");
    }
}
