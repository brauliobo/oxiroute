//! Stream table: a slab of small slots plus a sorted `(id, slot)` index; ids only grow so lookups are a short
//! binary search and a slot is found again in O(1) through the index cached in the application handle.
use std::collections::VecDeque;
use std::task::Waker;

use bytes::Bytes;
use http::HeaderMap;

use crate::error::{Initiator, Reason};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecvState {
    /// The peer may still send `DATA` or trailers.
    Open,
    /// `END_STREAM` received.
    Ended,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendState {
    /// No response `HEADERS` sent yet.
    Idle,
    /// Response `HEADERS` sent, body may follow.
    Open,
    /// `END_STREAM` sent.
    Ended,
}

/// A `DATA` payload waiting for the application; `budgeted` frames count against the small-frame budget.
pub struct Chunk {
    pub data: Bytes,
    pub budgeted: bool,
}

/// Request body state, allocated when the first DATA frame or the trailers arrive.
#[derive(Default)]
pub struct Body {
    pub chunks: VecDeque<Chunk>,
    pub trailers: Option<HeaderMap>,
}

#[derive(Default)]
pub struct Wakers {
    pub recv: Option<Waker>,
    pub send: Option<Waker>,
    pub reset: Option<Waker>,
}

#[allow(clippy::struct_excessive_bools)]
pub struct Slot {
    pub id: u32,
    pub recv: RecvState,
    pub send: SendState,
    /// Reset by either side; frames of a reset stream are ignored.
    pub reset: Option<(Reason, Initiator)>,
    /// Handed to the application (a `Stream` handle exists).
    pub accepted: bool,
    /// Counted in the number of open streams (not both directions closed yet).
    pub open: bool,
    /// Parked in the connection-level capacity queue.
    pub waiting_conn: bool,
    pub send_window: i32,
    pub reserved: u32,
    pub recv_window: i32,
    /// Receive credit returned by the application and not yet sent as `WINDOW_UPDATE`.
    pub unreleased: u32,
    /// Octets of declared content-length still to receive; `u64::MAX` when undeclared.
    pub content_remaining: u64,
    pub body: Option<Box<Body>>,
    pub wakers: Wakers,
}

impl Slot {
    fn new(id: u32, send_window: i32, recv_window: i32) -> Self {
        Self {
            id,
            recv: RecvState::Open,
            send: SendState::Idle,
            reset: None,
            accepted: false,
            open: true,
            waiting_conn: false,
            send_window,
            reserved: 0,
            recv_window,
            unreleased: 0,
            content_remaining: u64::MAX,
            body: None,
            wakers: Wakers::default(),
        }
    }

    pub fn body(&mut self) -> &mut Body {
        self.body.get_or_insert_with(Box::default)
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.reset.is_some() || (self.recv == RecvState::Ended && self.send == SendState::Ended)
    }

    /// Wakes every task parked on this stream.
    pub fn wake_all(&mut self) {
        for w in [
            &mut self.wakers.recv,
            &mut self.wakers.send,
            &mut self.wakers.reset,
        ] {
            if let Some(w) = w.take() {
                w.wake();
            }
        }
    }
}

/// Slots a connection keeps while it has no stream.
const IDLE_SLOTS: usize = 16;

#[derive(Default)]
pub struct Streams {
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Sorted by id.
    index: Vec<(u32, u32)>,
}

impl Streams {
    fn position(&self, id: u32) -> Result<usize, usize> {
        self.index.binary_search_by_key(&id, |e| e.0)
    }

    /// Slot index for `id`.
    #[must_use]
    pub fn find(&self, id: u32) -> Option<u32> {
        self.position(id).ok().map(|p| self.index[p].1)
    }

    /// The slot at `idx` when it still belongs to `id` (handles cache the index).
    pub fn get(&mut self, idx: u32, id: u32) -> Option<&mut Slot> {
        self.slots.get_mut(idx as usize).filter(|s| s.id == id)
    }

    pub fn slot(&mut self, idx: u32) -> &mut Slot {
        &mut self.slots[idx as usize]
    }

    /// Inserts a stream whose id is larger than every id inserted before.
    pub fn insert(&mut self, id: u32, send_window: i32, recv_window: i32) -> u32 {
        let slot = Slot::new(id, send_window, recv_window);
        let idx = if let Some(idx) = self.free.pop() {
            self.slots[idx as usize] = slot;
            idx
        } else {
            self.slots.push(slot);
            (self.slots.len() - 1) as u32
        };
        self.index.push((id, idx));
        idx
    }

    pub fn remove(&mut self, id: u32) {
        if let Ok(p) = self.position(id) {
            let (_, idx) = self.index.remove(p);
            let slot = &mut self.slots[idx as usize];
            slot.id = 0;
            slot.body = None;
            slot.wakers = Wakers::default();
            self.free.push(idx);
            self.release_when_idle();
        }
    }

    /// A connection that has no stream left gives back the slots a burst of concurrent streams needed: a handle
    /// that outlives its stream finds no slot (or one that belongs to another id) and so does nothing.
    fn release_when_idle(&mut self) {
        if self.index.is_empty() && self.slots.capacity() > IDLE_SLOTS {
            self.slots = Vec::new();
            self.free = Vec::new();
            self.index = Vec::new();
        }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Slot> {
        self.slots.iter_mut().filter(|s| s.id != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_streams_leaves_no_slots_behind_once_they_are_gone() {
        let mut t = Streams::default();
        for id in (1..=200).step_by(2) {
            t.insert(id, 10, 10);
        }
        for id in (1..=200).step_by(2) {
            t.remove(id);
        }
        assert_eq!(
            t.slots.capacity() + t.free.capacity() + t.index.capacity(),
            0
        );
        let again = t.insert(201, 10, 10);
        assert_eq!(t.get(again, 201).map(|s| s.id), Some(201));
        assert!(t.get(again, 1).is_none());
        assert!(
            t.get(5, 3).is_none(),
            "a handle to a stream that is gone finds no slot"
        );
    }

    #[test]
    fn insert_find_remove_reuse() {
        let mut t = Streams::default();
        let a = t.insert(1, 10, 10);
        let b = t.insert(3, 10, 10);
        assert_eq!((t.find(1), t.find(3), t.find(5)), (Some(a), Some(b), None));
        t.remove(1);
        assert_eq!(t.find(1), None);
        assert!(t.get(a, 1).is_none());
        let c = t.insert(5, 10, 10);
        assert_eq!(c, a);
        assert_eq!(t.get(c, 5).map(|s| s.id), Some(5));
    }
}
