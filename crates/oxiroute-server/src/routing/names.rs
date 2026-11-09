//! Finds a pool member by name without hashing or copying the names.

use std::sync::Arc;

use super::{rendezvous, server::PoolEndpoint};

const EMPTY: u32 = u32::MAX;
/// Pools up to this size compare names in order, which beats building a table.
const SCANNED: usize = 16;

/// The positions of a membership by server name: a linear scan for a small pool, else an
/// open-addressed table of positions probed by the stable name hash and checked against the member.
#[derive(Debug)]
pub(super) struct NameIndex {
    slots: Box<[u32]>,
}

impl NameIndex {
    pub(super) fn new(members: &[Arc<PoolEndpoint>]) -> Self {
        if members.len() <= SCANNED {
            return Self {
                slots: Box::default(),
            };
        }
        let mut slots = vec![EMPTY; (members.len() * 2).next_power_of_two()].into_boxed_slice();
        let mask = slots.len() - 1;
        for (position, member) in (0..).zip(members) {
            let mut at = Self::start(&member.name, mask);
            while slots[at] != EMPTY {
                at = (at + 1) & mask;
            }
            slots[at] = position;
        }
        Self { slots }
    }

    fn start(name: &str, mask: usize) -> usize {
        usize::try_from(rendezvous::name_hash(name)).unwrap_or(0) & mask
    }

    pub(super) fn find(&self, members: &[Arc<PoolEndpoint>], name: &str) -> Option<usize> {
        if self.slots.is_empty() {
            return members.iter().position(|member| member.name == name);
        }
        let mask = self.slots.len() - 1;
        let mut at = Self::start(name, mask);
        while self.slots[at] != EMPTY {
            let position = self.slots[at] as usize;
            if members[position].name == name {
                return Some(position);
            }
            at = (at + 1) & mask;
        }
        None
    }
}
