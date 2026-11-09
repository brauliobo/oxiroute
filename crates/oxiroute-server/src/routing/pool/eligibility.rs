#[allow(clippy::wildcard_imports)]
use super::*;

/// What a server needs besides being selectable to be picked.
#[derive(Clone, Copy)]
pub(super) enum Capacity {
    /// Headroom for a new permit.
    Permit,
    /// Headroom for a new permit, or an idle pooled connection that needs none.
    PermitOrIdle,
    Any,
}

impl Capacity {
    pub(super) fn reusing_idle(reuse_idle: bool) -> Self {
        if reuse_idle {
            Self::PermitOrIdle
        } else {
            Self::Permit
        }
    }
}

/// The servers a selection may pick: not excluded by name, not already tried, with the capacity.
#[derive(Clone, Copy)]
pub(super) struct Eligibility<'a> {
    pub(super) excluded: &'a [String],
    pub(super) tried: &'a [usize],
    pub(super) capacity: Capacity,
}

impl<'a> Eligibility<'a> {
    pub(super) fn new(excluded: &'a [String], capacity: Capacity) -> Self {
        Self {
            excluded,
            tried: &[],
            capacity,
        }
    }

    pub(super) fn without(self, tried: &'a [usize]) -> Self {
        Self { tried, ..self }
    }

    fn names(&self, server: &PoolEndpoint, index: usize) -> bool {
        (self.excluded.is_empty() || !self.excluded.contains(&server.name))
            && !self.tried.contains(&index)
    }

    fn has_capacity(&self, server: &PoolEndpoint) -> bool {
        match self.capacity {
            Capacity::Permit => server.has_capacity(),
            Capacity::PermitOrIdle => server.has_capacity() || server.has_idle_connection(),
            Capacity::Any => true,
        }
    }

    pub(super) fn admits(&self, members: &Membership, index: usize) -> bool {
        let server = &members.endpoints[index];
        self.names(server, index) && self.has_capacity(server)
    }

    /// Whether the server would be eligible if it had the capacity.
    pub(super) fn saturates(&self, members: &Membership, index: usize) -> bool {
        let server = &members.endpoints[index];
        self.names(server, index) && !self.has_capacity(server)
    }

    /// The load of an eligible server, else `None`; reads the server's counters once.
    pub(super) fn load(&self, members: &Membership, index: usize) -> Option<u64> {
        let server = &members.endpoints[index];
        if !self.names(server, index) {
            return None;
        }
        match self.capacity {
            Capacity::Permit => server.load_if_open(false),
            Capacity::PermitOrIdle => server.load_if_open(true),
            Capacity::Any => Some(server.load()),
        }
    }
}
