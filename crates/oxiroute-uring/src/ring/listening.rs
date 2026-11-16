use std::os::fd::OwnedFd;

use rustix::io::Errno;

use super::Ring;
use crate::{
    addr::Address,
    event::Event,
    raw::{Completion, Memory},
    slot::{Acceptor, Handle, Kind, Meta},
};

impl Ring {
    /// Adopts a listening socket. It accepts nothing until [`Ring::set_accepting`] enables it.
    pub fn listen(&mut self, fd: OwnedFd, tag: u64) -> Handle {
        let handle = self.adopt(fd, tag);
        let accepts = usize::from(self.config.accepts);
        if let Some(slot) = self.slot(handle) {
            slot.acceptor = Some(Acceptor {
                enabled: false,
                flying: vec![None; accepts],
                spare: Vec::new(),
                held: Vec::new(),
            });
        }
        handle
    }

    /// Turns accepting on or off. Connections accepted while it is off wait inside the ring and are delivered
    /// when it is turned on again, as if they had waited in the kernel's accept queue.
    pub fn set_accepting(&mut self, handle: Handle, enabled: bool) {
        let Some(acceptor) = self.slot(handle).and_then(|slot| slot.acceptor.as_mut()) else {
            return;
        };
        acceptor.enabled = enabled;
        let held = if enabled {
            std::mem::take(&mut acceptor.held)
        } else {
            Vec::new()
        };
        self.pending.extend(held);
        if enabled {
            self.post_accepts(handle);
        } else {
            self.cancel_accepts(handle);
        }
    }

    fn free_accepts(&mut self, handle: Handle) -> Vec<usize> {
        let Some(slot) = self.slot(handle).filter(|slot| !slot.closing) else {
            return Vec::new();
        };
        match slot.acceptor.as_ref().filter(|acceptor| acceptor.enabled) {
            Some(acceptor) => (0..acceptor.flying.len())
                .filter(|sub| acceptor.flying[*sub].is_none())
                .collect(),
            None => Vec::new(),
        }
    }

    pub(crate) fn post_accepts(&mut self, handle: Handle) {
        for sub in self.free_accepts(handle) {
            if !self.post_accept(handle, sub) {
                self.stalled_accepts.push(handle);
                return;
            }
        }
    }

    fn post_accept(&mut self, handle: Handle, sub: usize) -> bool {
        let storage = self
            .slot(handle)
            .and_then(|slot| slot.acceptor.as_mut())
            .and_then(|acceptor| acceptor.spare.pop())
            .unwrap_or_else(|| Box::new(Address::empty()));
        let meta = Self::meta(Kind::Accept, sub as u8, handle);
        let Some(Ok(key)) = self.with_fd(handle, |queue, fd| queue.accept(fd, storage, meta))
        else {
            return false;
        };
        if let Some(slot) = self.slot(handle) {
            slot.inflight += 1;
            if let Some(acceptor) = slot.acceptor.as_mut() {
                acceptor.flying[sub] = Some(key);
            }
        }
        true
    }

    pub(crate) fn cancel_accepts(&mut self, handle: Handle) {
        let keys: Vec<u64> = self
            .slot(handle)
            .and_then(|slot| slot.acceptor.as_ref())
            .map(|acceptor| acceptor.flying.iter().flatten().copied().collect())
            .unwrap_or_default();
        for key in keys {
            let _ = self.queue.cancel(key);
        }
    }

    pub(crate) fn accepted(&mut self, meta: Meta, done: Completion) {
        let Memory::Address(storage) = done.memory else {
            return;
        };
        let peer = storage.socket();
        let Some(slot) = self.slot_any(meta.handle) else {
            return;
        };
        let (tag, closing) = (slot.tag, slot.closing);
        let Some(acceptor) = slot.acceptor.as_mut() else {
            return;
        };
        acceptor.flying[usize::from(meta.sub)] = None;
        acceptor.spare.push(storage);
        let enabled = acceptor.enabled;
        let failed = Self::errno(done.result).is_some_and(|error| error != Errno::CANCELED);
        if failed {
            self.stalled_accepts.push(meta.handle);
        }
        if let Some(fd) = done.accepted.filter(|_| !closing) {
            self.deliver(meta.handle, enabled, Event::Accepted { tag, fd, peer });
        }
        if !failed {
            self.post_accepts(meta.handle);
        }
    }

    fn deliver(&mut self, handle: Handle, enabled: bool, event: Event) {
        if enabled {
            return self.pending.push(event);
        }
        if let Some(acceptor) = self
            .slot_any(handle)
            .and_then(|slot| slot.acceptor.as_mut())
        {
            acceptor.held.push(event);
        }
    }

    pub(crate) fn retry_stalled_accepts(&mut self) {
        for handle in std::mem::take(&mut self.stalled_accepts) {
            self.post_accepts(handle);
        }
    }
}
