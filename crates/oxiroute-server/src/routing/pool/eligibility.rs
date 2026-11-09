#![allow(clippy::inline_always)] // the per-server checks run once per server on every pass
#[allow(clippy::wildcard_imports)]
use super::*;

/// What a server needs besides being selectable to be picked.
#[derive(Clone, Copy)]
pub(in crate::routing) enum Capacity {
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

    #[inline(always)]
    fn names(&self, server: &PoolEndpoint, index: usize) -> bool {
        (self.excluded.is_empty() || !self.excluded.contains(&server.name))
            && !self.tried.contains(&index)
    }

    #[inline(always)]
    fn has_capacity(&self, server: &PoolEndpoint) -> bool {
        match self.capacity {
            Capacity::Permit => server.has_capacity(),
            Capacity::PermitOrIdle => server.has_capacity() || server.has_idle_connection(),
            Capacity::Any => true,
        }
    }

    #[inline(always)]
    pub(super) fn admits(&self, members: &Membership, index: usize) -> bool {
        let server = &members.endpoints[index];
        self.names(server, index) && self.has_capacity(server)
    }

    /// Whether the server would be eligible if it had the capacity. After a failed choice every such
    /// server was passed over for its capacity; reading the capacity again would race with releases.
    #[inline(always)]
    pub(super) fn saturates(&self, members: &Membership, index: usize) -> bool {
        self.names(&members.endpoints[index], index)
    }

    /// The positions of the servers excluded by name.
    pub(super) fn excluded_positions(&self, members: &Membership) -> Vec<usize> {
        let positions = self.excluded.iter().map(|name| members.position(name));
        positions.flatten().collect()
    }

    /// The membership positions this selection must not pick: the excluded and the tried servers.
    pub(super) fn masked(&self, members: &Membership) -> Vec<usize> {
        let mut masked = self.excluded_positions(members);
        masked.extend_from_slice(self.tried);
        masked
    }
}
