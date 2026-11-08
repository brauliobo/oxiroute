//! Hand-off between request tasks and the access-log writer thread.
//!
//! Requests append finished lines to one shared buffer; the writer swaps the whole buffer out and
//! writes it with a single call. Appending allocates nothing, and the writer is only woken for the
//! first line of a batch, then waits a short window so concurrent requests join the same write.

use std::{
    sync::{Condvar, Mutex, MutexGuard},
    time::Duration,
};

/// Lines that may wait for the writer before new ones are rejected.
pub(crate) const QUEUE_CAPACITY_LINES: u64 = 1_024;

/// How long a log line may wait so that concurrent requests share one write.
const BATCH_WINDOW: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Rejected {
    Full,
    Stopped,
}

#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    lines: u64,
    closed: bool,
    failed: bool,
}

pub(crate) struct Sink {
    capacity: u64,
    pending: Mutex<Pending>,
    wake: Condvar,
}

impl Sink {
    pub(crate) fn new(capacity: u64) -> Self {
        Self {
            capacity,
            pending: Mutex::default(),
            wake: Condvar::new(),
        }
    }

    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queues one newline-terminated line.
    pub(super) fn push(&self, line: &[u8]) -> Result<(), Rejected> {
        let mut pending = self.pending();
        if pending.failed || pending.closed {
            return Err(Rejected::Stopped);
        }
        if pending.lines >= self.capacity {
            return Err(Rejected::Full);
        }
        pending.bytes.extend_from_slice(line);
        pending.lines += 1;
        let first = pending.lines == 1;
        drop(pending);
        if first {
            self.wake.notify_one();
        }
        Ok(())
    }

    /// Lets the writer finish what is queued and stop.
    pub(super) fn close(&self) {
        self.pending().closed = true;
        self.wake.notify_one();
    }

    /// Rejects every later line after a write error and returns how many queued lines were lost.
    pub(super) fn fail(&self) -> u64 {
        let mut pending = self.pending();
        pending.failed = true;
        pending.bytes.clear();
        std::mem::take(&mut pending.lines)
    }

    /// Waits for lines, gives concurrent requests the batch window to join them, then swaps them into
    /// `batch` and returns their count. `None` once the sink is closed and drained.
    pub(super) fn next_batch(&self, batch: &mut Vec<u8>) -> Option<u64> {
        let mut pending = self.pending();
        while pending.lines == 0 {
            if pending.closed {
                return None;
            }
            pending = self
                .wake
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        pending = self
            .wake
            .wait_timeout_while(pending, BATCH_WINDOW, |pending| !pending.closed)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
        batch.clear();
        std::mem::swap(batch, &mut pending.bytes);
        Some(std::mem::take(&mut pending.lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_lines_are_handed_over_as_one_batch_in_order() {
        let sink = Sink::new(QUEUE_CAPACITY_LINES);
        for index in 0..50 {
            sink.push(format!("line {index}\n").as_bytes())
                .expect("queued");
        }
        let mut batch = Vec::new();
        assert_eq!(sink.next_batch(&mut batch), Some(50));
        assert_eq!(batch.split(|byte| *byte == b'\n').count(), 51);
        assert!(batch.starts_with(b"line 0\nline 1\n"));
        sink.push(b"next\n").expect("queued after the swap");
        assert_eq!(sink.next_batch(&mut batch), Some(1));
        assert_eq!(batch, b"next\n");
    }

    #[test]
    fn a_full_queue_rejects_without_blocking_and_a_failed_one_stops() {
        let sink = Sink::new(2);
        assert_eq!(sink.push(b"a\n"), Ok(()));
        assert_eq!(sink.push(b"b\n"), Ok(()));
        assert_eq!(sink.push(b"c\n"), Err(Rejected::Full));
        assert_eq!(sink.fail(), 2);
        assert_eq!(sink.push(b"d\n"), Err(Rejected::Stopped));
    }

    #[test]
    fn closing_drains_what_is_queued_before_the_writer_stops() {
        let sink = Sink::new(4);
        sink.push(b"last\n").expect("queued");
        sink.close();
        let mut batch = Vec::new();
        assert_eq!(sink.next_batch(&mut batch), Some(1));
        assert_eq!(batch, b"last\n");
        assert_eq!(sink.next_batch(&mut batch), None);
    }
}
