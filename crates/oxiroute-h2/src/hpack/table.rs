//! The dynamic table (RFC 7541 section 2.3.2, 4): entries are slots of a ring, their bytes live in one arena.
//!
//! Entry positions are absolute offsets in the stream of all bytes ever appended, kept as wrapping `u32`; the
//! arena holds the window that starts at `base`. Evicted entries leave dead bytes at the front of the window,
//! which are dropped in bulk once they outweigh the live bytes, so appends and evictions are amortised O(1).

/// Per-entry overhead of the table size (RFC 7541 section 4.1).
pub(super) const ENTRY_OVERHEAD: usize = 32;
/// Largest table size honoured; keeps arena offsets inside `u32`.
pub(super) const MAX_SIZE: usize = 1 << 28;
const MIN_SLOTS: usize = 16;
const MIN_RECLAIM: usize = 512;

use super::bytes_eq::eq;

#[derive(Clone, Copy, Default)]
struct Entry {
    start: u32,
    name_len: u32,
    value_len: u32,
}

impl Entry {
    fn size(self) -> usize {
        self.name_len as usize + self.value_len as usize + ENTRY_OVERHEAD
    }
}

/// Where a field sits in the dynamic table; indices count from the newest entry, starting at 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Found {
    pub full: Option<usize>,
    pub name: Option<usize>,
}

pub(super) struct Table {
    slots: Vec<Entry>,
    oldest: usize,
    count: usize,
    arena: Vec<u8>,
    base: u32,
    size: usize,
    max: usize,
}

impl Table {
    pub(super) fn new(max: usize) -> Self {
        Self {
            slots: Vec::new(),
            oldest: 0,
            count: 0,
            arena: Vec::new(),
            base: 0,
            size: 0,
            max: max.min(MAX_SIZE),
        }
    }

    pub(super) fn max(&self) -> usize {
        self.max
    }

    /// Sum of entry sizes (RFC 7541 section 4.1).
    pub(super) fn size(&self) -> usize {
        self.size
    }

    pub(super) fn set_max(&mut self, max: usize) {
        self.max = max.min(MAX_SIZE);
        self.evict_down_to(self.max);
    }

    pub(super) fn clear(&mut self) {
        self.evict_down_to(0);
    }

    /// The entry `n` places back from the newest (1 is the newest, HPACK index 62).
    #[inline]
    pub(super) fn get(&self, n: usize) -> Option<(&[u8], &[u8])> {
        if n == 0 || n > self.count {
            return None;
        }
        Some(self.bytes(self.slots[self.slot(self.count - n)]))
    }

    /// Adds an entry after evicting as many old ones as needed; an entry larger than the whole table empties it.
    pub(super) fn insert(&mut self, name: &[u8], value: &[u8]) {
        let need = name.len() + value.len() + ENTRY_OVERHEAD;
        self.evict_down_to(self.max.saturating_sub(need));
        if need > self.max {
            return;
        }
        self.reclaim_dead_bytes();
        if self.count == self.slots.len() {
            self.grow();
        }
        let entry = Entry {
            start: self.base.wrapping_add(self.arena.len() as u32),
            name_len: name.len() as u32,
            value_len: value.len() as u32,
        };
        if self.arena.capacity() == 0 {
            self.arena.reserve(self.max.min(4096));
        }
        self.arena.extend_from_slice(name);
        self.arena.extend_from_slice(value);
        let slot = self.slot(self.count);
        self.slots[slot] = entry;
        self.count += 1;
        self.size += need;
    }

    /// Newest entry with this name and value, and newest with this name.
    pub(super) fn find(&self, name: &[u8], value: &[u8]) -> Found {
        let mut found = Found::default();
        for n in 1..=self.count {
            let entry = self.slots[self.slot(self.count - n)];
            if entry.name_len as usize != name.len() {
                continue;
            }
            let (entry_name, entry_value) = self.bytes(entry);
            if !eq(entry_name, name) {
                continue;
            }
            found.name.get_or_insert(n);
            if eq(entry_value, value) {
                found.full = Some(n);
                break;
            }
        }
        found
    }

    #[inline]
    fn slot(&self, from_oldest: usize) -> usize {
        (self.oldest + from_oldest) & (self.slots.len() - 1)
    }

    #[inline]
    fn bytes(&self, entry: Entry) -> (&[u8], &[u8]) {
        let start = entry.start.wrapping_sub(self.base) as usize;
        let name_end = start + entry.name_len as usize;
        (
            &self.arena[start..name_end],
            &self.arena[name_end..name_end + entry.value_len as usize],
        )
    }

    fn evict_down_to(&mut self, limit: usize) {
        while self.size > limit {
            self.size -= self.slots[self.oldest].size();
            self.oldest = (self.oldest + 1) & (self.slots.len() - 1);
            self.count -= 1;
        }
        if self.count == 0 {
            self.base = self.base.wrapping_add(self.arena.len() as u32);
            self.arena.clear();
        }
    }

    fn reclaim_dead_bytes(&mut self) {
        if self.count == 0 {
            return;
        }
        let dead = self.slots[self.oldest].start.wrapping_sub(self.base) as usize;
        if dead >= MIN_RECLAIM && dead > self.arena.len() - dead {
            self.arena.drain(..dead);
            self.base = self.base.wrapping_add(dead as u32);
        }
    }

    fn grow(&mut self) {
        let capacity = (self.slots.len() * 2).max(MIN_SLOTS);
        let mut slots: Vec<Entry> = (0..self.count).map(|i| self.slots[self.slot(i)]).collect();
        slots.resize(capacity, Entry::default());
        self.slots = slots;
        self.oldest = 0;
    }
}
