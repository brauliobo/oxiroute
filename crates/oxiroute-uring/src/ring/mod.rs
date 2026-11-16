mod closing;
mod listening;
mod reading;
mod writing;

use std::{
    collections::VecDeque,
    io,
    marker::PhantomData,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    rc::Rc,
    time::{Duration, Instant},
};

use rustix::io::Errno;

use crate::{
    config::Config,
    event::Event,
    raw::{Completion, Queue},
    slot::{Handle, Kind, Meta, Slot},
};

/// One `io_uring` per thread, used only from the thread that created it (`Ring` is neither `Send` nor `Sync`).
///
/// The ring owns every socket adopted into it and every buffer an operation uses. Completions are translated into
/// [`Event`]s; operations are never lost, never touch memory the caller still owns, and survive the caller
/// forgetting the socket: [`Ring::close`] cancels what is waiting, lets an accepted send finish and then closes.
pub struct Ring {
    queue: Queue,
    config: Config,
    slots: Vec<Slot>,
    free: Vec<u32>,
    pending: Vec<Event>,
    completions: Vec<Completion>,
    stagings: Vec<Vec<u8>>,
    starved: VecDeque<Handle>,
    stalled_accepts: Vec<Handle>,
    spare: Option<OwnedFd>,
    available: u16,
    spills: Vec<Vec<u8>>,
    zombies: VecDeque<(Instant, Handle)>,
    _single_issuer: PhantomData<Rc<()>>,
}

impl Ring {
    pub fn new(config: &Config) -> io::Result<Self> {
        config.validate()?;
        let queue = Queue::new(
            config.entries,
            config.taskrun,
            config.buffers,
            config.buffer_size,
        )?;
        Ok(Self {
            queue,
            config: config.clone(),
            slots: Vec::new(),
            free: Vec::new(),
            pending: Vec::new(),
            completions: Vec::new(),
            stagings: Vec::new(),
            starved: VecDeque::new(),
            stalled_accepts: Vec::new(),
            spare: Self::spare_descriptor(),
            available: config.buffers,
            spills: Vec::new(),
            zombies: VecDeque::new(),
            _single_issuer: PhantomData,
        })
    }

    pub(crate) fn opcodes(&self) -> io::Result<io_uring::Probe> {
        self.queue.probe()
    }

    pub(crate) fn with_fd<R>(
        &mut self,
        handle: Handle,
        submit: impl FnOnce(&mut Queue, BorrowedFd<'_>) -> R,
    ) -> Option<R> {
        let slot = self
            .slots
            .get_mut(handle.index as usize)
            .filter(|slot| slot.live && slot.generation == handle.generation)?;
        Some(submit(&mut self.queue, slot.fd.as_ref()?.as_fd()))
    }

    /// Adopts a descriptor. The ring closes it when the socket is closed and no operation uses it any more.
    pub fn adopt(&mut self, fd: OwnedFd, tag: u64) -> Handle {
        let index = self.free.pop().unwrap_or_else(|| {
            self.slots.push(Slot::vacant());
            u32::try_from(self.slots.len() - 1).expect("the slot table fits 32 bits")
        });
        let slot = &mut self.slots[index as usize];
        slot.open(fd, tag);
        Handle {
            index,
            generation: slot.generation,
        }
    }

    pub(crate) fn slot(&mut self, handle: Handle) -> Option<&mut Slot> {
        self.slots
            .get_mut(handle.index as usize)
            .filter(|slot| slot.live && slot.generation == handle.generation)
    }

    pub(crate) fn meta(kind: Kind, sub: u8, handle: Handle) -> u64 {
        Meta { kind, sub, handle }.pack()
    }

    pub(crate) fn emit(&mut self, handle: Handle, event: impl FnOnce(u64) -> Event) {
        if let Some(slot) = self.slot(handle).filter(|slot| !slot.closing) {
            let event = event(slot.tag);
            self.pending.push(event);
        }
    }

    /// Hands queued operations to the kernel, waits for a completion or until `timeout` passes (`None` waits
    /// without limit) and appends what happened to `events`.
    pub fn wait(&mut self, timeout: Option<Duration>, events: &mut Vec<Event>) -> io::Result<()> {
        let timeout = if self.pending.is_empty() {
            self.bounded(timeout)
        } else {
            Some(Duration::ZERO)
        };
        self.queue.wait(timeout)?;
        self.settle();
        events.append(&mut self.pending);
        Ok(())
    }

    pub(crate) fn settle(&mut self) {
        let mut completions = std::mem::take(&mut self.completions);
        self.queue.reap(&mut completions);
        let progress = completions
            .iter()
            .any(|done| Meta::unpack(done.meta).kind != Kind::Accept);
        for done in completions.drain(..) {
            self.complete(done);
        }
        self.completions = completions;
        self.expire_zombies();
        self.rearm_starved();
        if progress {
            self.recover_accepts();
        }
    }

    fn complete(&mut self, done: Completion) {
        let meta = Meta::unpack(done.meta);
        let Some(slot) = self.slot_any(meta.handle) else {
            return;
        };
        slot.inflight -= 1;
        match meta.kind {
            Kind::Recv => self.received(meta.handle, &done),
            Kind::Send => self.sent(meta.handle, done),
            Kind::Accept => self.accepted(meta, done),
            Kind::Connect => self.connected_to(meta.handle, done.result),
            Kind::Poll => self.polled(meta.handle),
        }
        self.finalize(meta.handle);
    }

    pub(crate) fn errno(result: i32) -> Option<Errno> {
        (result < 0).then(|| Errno::from_raw_os_error(-result))
    }

    pub(crate) fn slot_any(&mut self, handle: Handle) -> Option<&mut Slot> {
        self.slots
            .get_mut(handle.index as usize)
            .filter(|slot| slot.generation == handle.generation && slot.live)
    }

    pub(crate) fn take_staging(&mut self) -> Vec<u8> {
        self.stagings.pop().unwrap_or_default()
    }

    pub(crate) fn give_staging(&mut self, mut staging: Vec<u8>) {
        staging.clear();
        if self.stagings.len() < 1024 {
            self.stagings.push(staging);
        }
    }

    /// Operations the kernel still holds; zero once every closed socket has finished.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.queue.in_flight()
    }

    /// The buffer size and count of the provided-buffer ring, for tests and reports.
    #[must_use]
    pub fn buffer_geometry(&self) -> (u16, u32) {
        (self.config.buffers, self.config.buffer_size)
    }
}
