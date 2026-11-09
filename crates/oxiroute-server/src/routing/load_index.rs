//! An index of server loads, so least connections picks without looking at every server.
//!
//! The index keeps one tournament tree per kind of capacity a selection may require; the tree holds
//! the load of each selectable server (or `OFF` for a server that cannot take the work), so the least
//! loaded server is the root and the first one in rotation order among equals is found by descent.
//! Loads change when leases start and end, so whoever changes a server's load notes the server in
//! the pool's [`LoadLog`], and a selection brings the trees up to date from the log before it reads.

use std::sync::{Arc, Mutex, PoisonError, atomic::Ordering};

use super::{Membership, PoolEndpoint, SelectableView, pool::Capacity};

/// The key of a server that cannot take the work.
const OFF: u64 = u64::MAX;

impl Capacity {
    pub(super) const KINDS: usize = 3;
    const ALL: [Self; Self::KINDS] = [Self::Permit, Self::PermitOrIdle, Self::Any];

    pub(super) const fn kind(self) -> usize {
        match self {
            Self::Permit => 0,
            Self::PermitOrIdle => 1,
            Self::Any => 2,
        }
    }

    /// The load of a server that has this capacity, else `None`; reads the counters once.
    #[inline]
    pub(super) fn load_of(self, server: &PoolEndpoint) -> Option<u64> {
        match self {
            Self::Permit => server.load_if_open(false),
            Self::PermitOrIdle => server.load_if_open(true),
            Self::Any => Some(server.load()),
        }
    }
}

/// The servers whose load changed since the pool's index last looked.
#[derive(Debug)]
pub(super) struct LoadLog {
    tracked: bool,
    noted: Mutex<Vec<Arc<PoolEndpoint>>>,
}

impl LoadLog {
    /// A log that records only when the pool's algorithm reads loads from an index.
    pub(super) fn new(tracked: bool) -> Self {
        Self {
            tracked,
            noted: Mutex::new(Vec::new()),
        }
    }

    /// Records that the load or the capacity of `server` changed.
    pub(super) fn note(&self, server: &Arc<PoolEndpoint>) {
        if self.tracked && !server.load_noted.swap(true, Ordering::AcqRel) {
            self.noted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Arc::clone(server));
        }
    }

    /// Moves the noted servers into `out`, which is emptied first, and arms them to be noted again.
    /// A change after this point is noted again, a change before it is read by the caller.
    pub(super) fn take(&self, out: &mut Vec<Arc<PoolEndpoint>>) {
        out.clear();
        std::mem::swap(
            out,
            &mut *self.noted.lock().unwrap_or_else(PoisonError::into_inner),
        );
        for server in out.iter() {
            server.load_noted.store(false, Ordering::Release);
        }
    }
}

/// The loads of the selectable servers by membership position, as a tournament tree.
#[derive(Debug)]
pub(super) struct LoadTree {
    leaves: usize,
    /// Node 1 is the root, the children of `n` are `2n` and `2n + 1`, the leaves start at `leaves`.
    nodes: Box<[u64]>,
}

impl LoadTree {
    fn build(members: &Membership, view: &SelectableView, capacity: Capacity) -> Self {
        let mut keys = vec![OFF; members.endpoints.len()];
        for entry in &view.entries {
            let at = entry.index as usize;
            keys[at] = Self::key(&members.endpoints[at], capacity);
        }
        Self::from_keys(&keys)
    }

    fn from_keys(keys: &[u64]) -> Self {
        let leaves = keys.len().next_power_of_two();
        let mut nodes = vec![OFF; 2 * leaves].into_boxed_slice();
        nodes[leaves..leaves + keys.len()].copy_from_slice(keys);
        for node in (1..leaves).rev() {
            nodes[node] = nodes[2 * node].min(nodes[2 * node + 1]);
        }
        Self { leaves, nodes }
    }

    fn key(server: &PoolEndpoint, capacity: Capacity) -> u64 {
        capacity.load_of(server).unwrap_or(OFF)
    }

    fn get(&self, at: usize) -> u64 {
        self.nodes[self.leaves + at]
    }

    fn set(&mut self, at: usize, key: u64) {
        let mut node = self.leaves + at;
        self.nodes[node] = key;
        while node > 1 {
            node /= 2;
            let least = self.nodes[2 * node].min(self.nodes[2 * node + 1]);
            if self.nodes[node] == least {
                return;
            }
            self.nodes[node] = least;
        }
    }

    /// The first position from `from` on whose key is at most `bound`.
    fn first_from(&self, from: usize, bound: u64) -> Option<usize> {
        let mut node = self.leaves + from;
        loop {
            if self.nodes[node] <= bound {
                while node < self.leaves {
                    node *= 2;
                    node += usize::from(self.nodes[node] > bound);
                }
                return Some(node - self.leaves);
            }
            while node & 1 == 1 {
                node /= 2;
            }
            if node == 0 {
                return None;
            }
            node += 1;
        }
    }

    /// The least loaded position outside `masked`, the first from `start` in rotation among equals.
    pub(super) fn least(&mut self, start: usize, masked: &[usize]) -> Option<usize> {
        let kept = masked
            .iter()
            .map(|&at| (at, self.get(at)))
            .collect::<Vec<_>>();
        for &(at, _) in &kept {
            self.set(at, OFF);
        }
        let least = self.nodes[1];
        let found = (least != OFF)
            .then(|| {
                self.first_from(start, least)
                    .or_else(|| self.first_from(0, least))
            })
            .flatten();
        for &(at, key) in kept.iter().rev() {
            self.set(at, key);
        }
        found
    }
}

/// What the pool's selections keep between calls: the trees of one published view of the members.
#[derive(Debug, Default)]
pub(super) struct LoadIndex {
    view: u64,
    trees: [Option<LoadTree>; Capacity::KINDS],
    noted: Vec<Arc<PoolEndpoint>>,
}

impl LoadIndex {
    /// The tree for `capacity` over `view`, current with every load change noted so far.
    pub(super) fn tree(
        &mut self,
        log: &LoadLog,
        members: &Membership,
        view: &SelectableView,
        capacity: Capacity,
    ) -> &mut LoadTree {
        log.take(&mut self.noted);
        if self.view == view.id() {
            self.apply_noted(members, view);
        } else {
            self.view = view.id();
            self.trees = Default::default();
        }
        self.trees[capacity.kind()].get_or_insert_with(|| LoadTree::build(members, view, capacity))
    }

    fn apply_noted(&mut self, members: &Membership, view: &SelectableView) {
        for server in &self.noted {
            let at = server.slot.load(Ordering::Acquire) as usize;
            let member = members
                .endpoints
                .get(at)
                .is_some_and(|member| Arc::ptr_eq(member, server));
            if !member || !view.contains(at) {
                continue;
            }
            for (kind, tree) in self.trees.iter_mut().enumerate() {
                if let Some(tree) = tree {
                    tree.set(at, LoadTree::key(server, Capacity::ALL[kind]));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The least key outside `masked`, the first from `start` in rotation among equals.
    fn scan(keys: &[u64], start: usize, masked: &[usize]) -> Option<usize> {
        let rotation = (0..keys.len()).map(|offset| (start + offset) % keys.len());
        rotation
            .filter(|at| !masked.contains(at) && keys[*at] != OFF)
            .fold(None, |best: Option<usize>, at| {
                best.filter(|&best| keys[best] <= keys[at]).or(Some(at))
            })
    }

    #[test]
    fn the_tree_finds_what_a_rotation_scan_finds_after_any_update() {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut random = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % bound as u64).expect("bounded")
        };
        for size in [1, 2, 3, 5, 8, 13, 64, 100] {
            let mut keys = (0..size).map(|_| random(4) as u64).collect::<Vec<_>>();
            let mut tree = LoadTree::from_keys(&keys);
            for _ in 0..400 {
                let at = random(size);
                keys[at] = if random(5) == 0 {
                    OFF
                } else {
                    random(4) as u64
                };
                tree.set(at, keys[at]);
                let masked = (0..random(3)).map(|_| random(size)).collect::<Vec<_>>();
                let start = random(size);
                assert_eq!(tree.least(start, &masked), scan(&keys, start, &masked));
            }
            assert!(
                keys.iter()
                    .enumerate()
                    .all(|(at, key)| tree.get(at) == *key)
            );
        }
    }
}
