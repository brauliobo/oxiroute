#[cfg(target_os = "linux")]
use std::{
    io,
    os::fd::{AsFd as _, BorrowedFd, OwnedFd},
    sync::OnceLock,
};
use std::{
    sync::{Condvar, Mutex, PoisonError},
    time::Instant,
};

static PROCESS: Doorbell = Doorbell::new();

/// What a supervising thread sleeps on: whoever changes something it watches rings it, so the thread looks again
/// when there is something to see and not on a timer.
///
/// A thread reads [`Doorbell::rung`] before it looks at what it watches and sleeps with that count, so a ring
/// that lands between looking and sleeping ends the sleep at once. A ring says that something changed, not what.
pub struct Doorbell {
    rings: Mutex<u64>,
    bell: Condvar,
    #[cfg(target_os = "linux")]
    wire: OnceLock<OwnedFd>,
}

impl Default for Doorbell {
    fn default() -> Self {
        Self::new()
    }
}

impl Doorbell {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rings: Mutex::new(0),
            bell: Condvar::new(),
            #[cfg(target_os = "linux")]
            wire: OnceLock::new(),
        }
    }

    /// The bell of this process: its supervisor sleeps on it.
    #[must_use]
    pub fn process() -> &'static Self {
        &PROCESS
    }

    pub fn ring(&self) {
        *self.rings.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.bell.notify_all();
        #[cfg(target_os = "linux")]
        if let Some(wire) = self.wire.get() {
            let _ = rustix::io::write(wire, &1_u64.to_ne_bytes());
        }
    }

    #[must_use]
    pub fn rung(&self) -> u64 {
        *self.rings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sleeps until the bell was rung after `heard` rings, or until `due` when there is one.
    pub fn sleep_after(&self, heard: u64, due: Option<Instant>) {
        let rings = self.rings.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(due) = due else {
            drop(
                self.bell
                    .wait_while(rings, |rings| *rings == heard)
                    .unwrap_or_else(PoisonError::into_inner),
            );
            return;
        };
        let wait = due.saturating_duration_since(Instant::now());
        drop(
            self.bell
                .wait_timeout_while(rings, wait, |rings| *rings == heard)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

#[cfg(target_os = "linux")]
impl Doorbell {
    /// A descriptor that polls readable from a ring until [`Doorbell::settle`], for a thread that also waits on
    /// other descriptors. Rings before the first call are not mirrored, so a thread asks for it before it looks.
    ///
    /// # Errors
    ///
    /// Returns an error when the descriptor cannot be created.
    pub fn pollable(&self) -> io::Result<BorrowedFd<'_>> {
        if let Some(wire) = self.wire.get() {
            return Ok(wire.as_fd());
        }
        let wire = rustix::event::eventfd(
            0,
            rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
        )?;
        Ok(self.wire.get_or_init(|| wire).as_fd())
    }

    /// Clears what [`Doorbell::pollable`] shows; the rings it showed are looked at after this.
    pub fn settle(&self) {
        if let Some(wire) = self.wire.get() {
            let _ = rustix::io::read(wire, &mut [0_u8; 8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn a_ring_before_the_sleep_ends_it_at_once() {
        let bell = Doorbell::new();
        let heard = bell.rung();
        bell.ring();
        bell.sleep_after(heard, None);
    }

    #[test]
    fn a_ring_from_another_thread_wakes_the_sleeper() {
        let bell = Arc::new(Doorbell::new());
        let heard = bell.rung();
        let ringer = {
            let bell = Arc::clone(&bell);
            thread::spawn(move || bell.ring())
        };
        bell.sleep_after(heard, None);
        ringer.join().expect("ringer");
    }

    #[test]
    fn a_sleep_with_a_due_time_ends_at_it_when_nobody_rings() {
        let bell = Doorbell::new();
        let due = Instant::now() + Duration::from_millis(30);
        bell.sleep_after(bell.rung(), Some(due));
        assert!(Instant::now() >= due);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_ring_makes_the_descriptor_readable_until_it_settles() {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let bell = Doorbell::new();
        let readable = || {
            let wire = bell.pollable().expect("descriptor");
            let mut wires = [PollFd::from_borrowed_fd(wire, PollFlags::IN)];
            let now = Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            poll(&mut wires, Some(&now)).expect("poll") > 0
        };
        assert!(!readable());
        bell.ring();
        assert!(readable());
        bell.settle();
        assert!(!readable());
    }

    #[test]
    fn a_due_time_in_the_past_does_not_sleep() {
        let bell = Doorbell::new();
        bell.sleep_after(bell.rung(), Some(Instant::now()));
    }
}
