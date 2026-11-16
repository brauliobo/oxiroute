use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use oxiroute_worker_scaling::Workers;

use super::arrival::Inbox;
use crate::plane::wake::Waker;

#[repr(align(64))]
#[derive(Default)]
struct Padded<T>(T);

struct Peer<A> {
    open: Padded<AtomicUsize>,
    busy_ns: Padded<AtomicU64>,
    inbox: Inbox<A>,
    wake: Waker,
}

pub(crate) struct Peers<A> {
    peers: Vec<Peer<A>>,
}

impl<A> Peers<A> {
    pub(crate) fn new(wakes: Vec<Waker>) -> io::Result<Self> {
        let peers = wakes
            .into_iter()
            .map(|wake| {
                Ok(Peer {
                    open: Padded::default(),
                    busy_ns: Padded::default(),
                    inbox: Inbox::new()?,
                    wake,
                })
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { peers })
    }

    pub(crate) fn len(&self) -> usize {
        self.peers.len()
    }

    pub(crate) fn open(&self, thread: usize) -> usize {
        self.peers[thread].open.0.load(Ordering::Relaxed)
    }

    pub(crate) fn opened(&self, thread: usize) {
        self.peers[thread].open.0.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn closed(&self, thread: usize) {
        self.peers[thread].open.0.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn busy(&self, thread: usize) -> u64 {
        self.peers[thread].busy_ns.0.load(Ordering::Relaxed)
    }

    pub(crate) fn worked(&self, thread: usize, nanos: u64) {
        let busy = &self.peers[thread].busy_ns.0;
        busy.store(
            busy.load(Ordering::Relaxed).saturating_add(nanos),
            Ordering::Relaxed,
        );
    }

    pub(crate) fn inbox(&self, thread: usize) -> &Inbox<A> {
        &self.peers[thread].inbox
    }

    pub(crate) fn wake(&self, thread: usize) {
        self.peers[thread].wake.wake();
    }
}

pub(crate) struct EngineWorkers<A>(pub(crate) Arc<Peers<A>>);

impl<A: Send + 'static> Workers for EngineWorkers<A> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn busy_nanos(&self, worker: usize) -> u64 {
        self.0.busy(worker)
    }

    fn connections(&self, worker: usize) -> usize {
        self.0.open(worker)
    }

    fn active_changed(&self, previous: usize, active: usize) {
        (previous.min(active)..previous.max(active)).for_each(|thread| self.0.wake(thread));
    }
}
