use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Who is waiting for capacity in one pool and since when, on every wait path.
#[derive(Debug, Default)]
pub(super) struct WaitLedger {
    next: AtomicU64,
    starts: Mutex<BTreeMap<u64, Instant>>,
}

/// A waiter's place in the ledger: its key and how many were waiting when it joined, itself included.
#[derive(Clone, Copy, Debug)]
pub(super) struct WaitTicket {
    pub(super) key: u64,
    pub(super) position: u64,
}

impl WaitLedger {
    fn starts(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Instant>> {
        self.starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn join(&self) -> WaitTicket {
        let key = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut starts = self.starts();
        starts.insert(key, Instant::now());
        WaitTicket {
            key,
            position: starts.len() as u64,
        }
    }

    pub(super) fn leave(&self, ticket: WaitTicket) -> Duration {
        self.starts()
            .remove(&ticket.key)
            .map_or(Duration::ZERO, |start| start.elapsed())
    }

    pub(super) fn oldest(&self) -> Duration {
        self.starts()
            .values()
            .min()
            .map_or(Duration::ZERO, Instant::elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_everyone_waiting_and_leaving_reports_the_wait() {
        let ledger = WaitLedger::default();
        let first = ledger.join();
        let second = ledger.join();
        assert_eq!((first.position, second.position), (1, 2));
        std::thread::sleep(Duration::from_millis(15));
        assert!(ledger.oldest() >= Duration::from_millis(15));
        assert!(ledger.leave(first) >= Duration::from_millis(15));
        assert_eq!(ledger.join().position, 2);
        ledger.leave(second);
        assert_eq!(ledger.leave(first), Duration::ZERO);
    }
}
