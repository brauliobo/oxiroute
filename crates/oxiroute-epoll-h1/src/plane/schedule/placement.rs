use std::sync::Arc;

use oxiroute_worker_scaling::{Scaling, least_loaded};

use super::{arrival::Arrival, peers::Peers};
use crate::plane::wake::Waker;

pub(crate) struct Placement<A> {
    peers: Arc<Peers<A>>,
    scaling: Option<Arc<Scaling>>,
    own: usize,
}

impl<A> Placement<A> {
    pub(crate) const fn new(
        peers: Arc<Peers<A>>,
        scaling: Option<Arc<Scaling>>,
        own: usize,
    ) -> Self {
        Self {
            peers,
            scaling,
            own,
        }
    }

    pub(crate) const fn own(&self) -> usize {
        self.own
    }

    pub(crate) const fn scales(&self) -> bool {
        self.scaling.is_some()
    }

    fn load(&self) -> impl Fn(usize) -> usize + '_ {
        |thread| self.peers.open(thread)
    }

    pub(crate) fn choose(&self) -> usize {
        let thread = match &self.scaling {
            Some(scaling) => scaling.place(self.own, self.load()),
            None => least_loaded(self.own, self.peers.len(), self.load()),
        };
        self.peers.opened(thread);
        if let Some(scaling) = &self.scaling {
            scaling.connection_placed();
        }
        thread
    }

    pub(crate) fn send(&self, thread: usize, arrival: Arrival<A>) {
        self.peers.inbox(thread).push(arrival);
    }

    pub(crate) fn take(&self, into: &mut Vec<Arrival<A>>) {
        self.peers.inbox(self.own).take(into);
    }

    pub(crate) fn settle_inbox(&self) {
        self.peers.inbox(self.own).waker().settle();
    }

    pub(crate) fn inbox_waker(&self) -> Waker {
        self.peers.inbox(self.own).waker().clone()
    }

    pub(crate) fn left(&self) {
        self.peers.closed(self.own);
    }

    pub(crate) fn worked(&self, nanos: u64) {
        self.peers.worked(self.own, nanos);
    }

    pub(crate) fn active(&self) -> bool {
        self.scaling
            .as_ref()
            .is_none_or(|scaling| self.own < scaling.active())
    }

    pub(crate) fn wants_relocation(&self) -> bool {
        self.scaling
            .as_ref()
            .is_some_and(|scaling| scaling.wants_relocation(self.own, self.peers.open(self.own)))
    }

    pub(crate) fn relocation(&self) -> Option<usize> {
        let scaling = self.scaling.as_ref()?;
        let thread = scaling.relocation(self.own, self.load())?;
        self.peers.closed(self.own);
        self.peers.opened(thread);
        scaling.note_relocated();
        Some(thread)
    }
}
