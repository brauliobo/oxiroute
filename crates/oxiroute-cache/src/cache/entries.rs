//! The cached representations of one shard and the two indexes the cache asks them through: the variants of one request
//! (a lookup and an insertion look at those only) and the order of last use (an eviction takes its head). Every
//! operation costs the same however many representations the shard holds; a scan over all of them, once per request
//! while holding the cache mutex, cost a million instructions per miss at the default capacity.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use super::{BaseKey, CacheKey, PreparedEntry};

pub(super) struct StoredEntry {
    pub(super) entry: Arc<PreparedEntry>,
    pub(super) last_access: u64,
}

#[derive(Default)]
pub(super) struct Entries {
    by_key: HashMap<CacheKey, StoredEntry>,
    variants: HashMap<BaseKey, Vec<CacheKey>>,
    /// Keys by the sequence number of their last use, which is unique: the least recently used comes first.
    by_access: BTreeMap<u64, CacheKey>,
}

impl Entries {
    pub(super) fn get(&self, key: &CacheKey) -> Option<&StoredEntry> {
        self.by_key.get(key)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&CacheKey, &StoredEntry)> {
        self.by_key.iter()
    }

    /// The representations stored for one base request key.
    pub(super) fn variants(&self, base: &BaseKey) -> &[CacheKey] {
        self.variants.get(base).map_or(&[], Vec::as_slice)
    }

    pub(super) fn least_recently_used(&self) -> Option<(&u64, &CacheKey)> {
        self.by_access.iter().next()
    }

    /// Stores `entry` as the most recent use. `access` must be newer than every stored sequence number.
    pub(super) fn insert(&mut self, entry: PreparedEntry, access: u64) {
        let key = entry.key.clone();
        self.remove(&key);
        self.variants
            .entry(key.base().clone())
            .or_default()
            .push(key.clone());
        self.by_access.insert(access, key.clone());
        self.by_key.insert(
            key,
            StoredEntry {
                entry: Arc::new(entry),
                last_access: access,
            },
        );
    }

    pub(super) fn remove(&mut self, key: &CacheKey) -> Option<StoredEntry> {
        let stored = self.by_key.remove(key)?;
        self.by_access.remove(&stored.last_access);
        if let Some(keys) = self.variants.get_mut(key.base()) {
            keys.retain(|variant| variant != key);
            if keys.is_empty() {
                self.variants.remove(key.base());
            }
        }
        Some(stored)
    }

    /// Marks the representation as used at `access`, which must be newer than every stored sequence number.
    pub(super) fn touch(&mut self, key: &CacheKey, access: u64) -> Option<&StoredEntry> {
        let stored = self.by_key.get_mut(key)?;
        self.by_access.remove(&stored.last_access);
        stored.last_access = access;
        self.by_access.insert(access, key.clone());
        Some(&*stored)
    }
}
