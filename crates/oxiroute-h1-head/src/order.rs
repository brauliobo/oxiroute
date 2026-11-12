//! The order `http::HeaderMap` iterates in, emulated without the map.
//!
//! The map keeps one entry per name (case-insensitive) in a vector, in order of the first occurrence; the further
//! values of a name hang off its entry in arrival order and are visited right after it. `remove` is a `swap_remove` of
//! the entry (the last entry moves into the hole), `insert` replaces the first value in place and drops the others,
//! `append` adds a value to the entry or a new entry at the end.

use crate::class::{Class, same_name};
use crate::edit::{Edit, MAX_EDITS, Name};
use crate::fields::{Header, MAX_HEADERS};

/// Items are the scanned headers (indices below [`MAX_HEADERS`]) and the values the edits add
/// (`MAX_HEADERS` plus the index of the edit).
pub const MAX_ITEMS: usize = MAX_HEADERS + MAX_EDITS;
const NONE: u8 = u8::MAX;

/// The headers and edits the item indices refer to.
#[derive(Clone, Copy)]
pub struct Items<'a> {
    pub buf: &'a [u8],
    pub headers: &'a [Header],
    pub edits: &'a [Edit<'a>],
}

impl Items<'_> {
    fn class(&self, item: u8) -> Class {
        match self.headers.get(usize::from(item)) {
            Some(h) => h.class,
            None => self.edits[usize::from(item) - MAX_HEADERS].name().class,
        }
    }

    fn name(&self, item: u8) -> &[u8] {
        match self.headers.get(usize::from(item)) {
            Some(h) => h.name(self.buf),
            None => self.edits[usize::from(item) - MAX_HEADERS].name().lower,
        }
    }

    fn same(&self, a: u8, b: u8) -> bool {
        same_name(self.name(a), self.class(a), self.name(b), self.class(b))
    }

    fn is(&self, item: u8, name: &Name<'_>) -> bool {
        same_name(self.name(item), self.class(item), name.lower, name.class)
    }
}

/// The entries of an emulated `HeaderMap`, each with the chain of its values.
#[derive(Clone, Debug)]
pub struct HeaderOrder {
    /// First item of each entry, in map order.
    entries: [u8; MAX_ITEMS],
    /// Next item of the same entry, per item.
    next: [u8; MAX_ITEMS],
    len: usize,
    /// Known classes that have an entry.
    present: u32,
}

impl HeaderOrder {
    /// The order after the scanned headers were appended one by one. `duplicate_names` and `present` are the scanner's
    /// facts; without duplicates the entries are the headers in arrival order and nothing is compared.
    #[must_use]
    #[inline]
    pub fn new(items: &Items<'_>, duplicate_names: bool, present: u32) -> Self {
        let mut order = Self {
            entries: [0; MAX_ITEMS],
            next: [NONE; MAX_ITEMS],
            len: 0,
            present,
        };
        let count = items.headers.len();
        if duplicate_names {
            for item in 0..count as u8 {
                order.append(items, item);
            }
        } else {
            for (slot, item) in order.entries.iter_mut().zip(0..count as u8) {
                *slot = item;
            }
            order.len = count;
        }
        order
    }

    fn find(&self, items: &Items<'_>, name: &Name<'_>) -> Option<usize> {
        if name.class != Class::Other && self.present & name.class.bit() == 0 {
            return None;
        }
        self.entries[..self.len]
            .iter()
            .position(|&entry| items.is(entry, name))
    }

    fn push(&mut self, item: u8, class: Class) {
        self.entries[self.len] = item;
        self.len += 1;
        if class != Class::Other {
            self.present |= class.bit();
        }
    }

    fn chain_to(&mut self, entry: usize, item: u8) {
        let mut last = self.entries[entry];
        while self.next[usize::from(last)] != NONE {
            last = self.next[usize::from(last)];
        }
        self.next[usize::from(last)] = item;
    }

    /// `HeaderMap::append` of `item`.
    fn append(&mut self, items: &Items<'_>, item: u8) {
        let class = items.class(item);
        let found = self.entries[..self.len]
            .iter()
            .position(|&entry| items.same(entry, item));
        match found {
            Some(entry) => self.chain_to(entry, item),
            None => self.push(item, class),
        }
    }

    /// Applies the edit at `index` of `items.edits`.
    pub fn apply(&mut self, items: &Items<'_>, index: usize) {
        let item = (MAX_HEADERS + index) as u8;
        match &items.edits[index] {
            Edit::Remove(name) => {
                if let Some(entry) = self.find(items, name) {
                    self.len -= 1;
                    self.entries[entry] = self.entries[self.len];
                    if name.class != Class::Other {
                        self.present &= !name.class.bit();
                    }
                }
            }
            Edit::Set(name, _) => match self.find(items, name) {
                Some(entry) => self.entries[entry] = item,
                None => self.push(item, name.class),
            },
            Edit::Add(name, _) => match self.find(items, name) {
                Some(entry) => self.chain_to(entry, item),
                None => self.push(item, name.class),
            },
        }
    }

    /// The order after all the edits of `items`, applied in sequence.
    #[must_use]
    #[inline]
    pub fn after_edits(items: &Items<'_>, duplicate_names: bool, present: u32) -> Self {
        let mut order = Self::new(items, duplicate_names, present);
        for index in 0..items.edits.len() {
            order.apply(items, index);
        }
        order
    }

    /// Visits the items in the order the map iterates them.
    #[inline]
    pub fn for_each(&self, mut visit: impl FnMut(u8)) {
        for &entry in &self.entries[..self.len] {
            let mut item = entry;
            loop {
                visit(item);
                item = self.next[usize::from(item)];
                if item == NONE {
                    break;
                }
            }
        }
    }
}
