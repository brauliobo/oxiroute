use std::{
    io,
    net::SocketAddr,
    os::fd::OwnedFd,
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use crate::plane::wake::Waker;

pub(crate) struct Arrival<A> {
    pub(crate) fd: OwnedFd,
    pub(crate) peer: SocketAddr,
    pub(crate) listener: usize,
    pub(crate) admission: A,
    pub(crate) wait: Option<Duration>,
}

pub(crate) struct Inbox<A> {
    waiting: Mutex<Vec<Arrival<A>>>,
    waker: Waker,
}

impl<A> Inbox<A> {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            waiting: Mutex::new(Vec::new()),
            waker: Waker::new()?,
        })
    }

    fn waiting(&self) -> MutexGuard<'_, Vec<Arrival<A>>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn waker(&self) -> &Waker {
        &self.waker
    }

    pub(crate) fn push(&self, arrival: Arrival<A>) {
        let mut waiting = self.waiting();
        let first = waiting.is_empty();
        waiting.push(arrival);
        drop(waiting);
        if first {
            self.waker.wake();
        }
    }

    pub(crate) fn take(&self, into: &mut Vec<Arrival<A>>) {
        std::mem::swap(&mut *self.waiting(), into);
    }
}
