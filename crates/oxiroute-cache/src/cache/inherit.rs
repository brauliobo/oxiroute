#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Inherited {
    pub offered: usize,
    pub kept: usize,
}

impl Cache {
    /// # Errors
    ///
    /// Returns an error when the caches use different clocks.
    pub fn inherit(&self, from: &Self) -> Result<Inherited, CacheError> {
        if Arc::ptr_eq(&self.shared, &from.shared) {
            let held = from.shared.usage.entries.load(Ordering::Relaxed);
            return Ok(Inherited {
                offered: held,
                kept: held,
            });
        }
        if !Arc::ptr_eq(&self.shared.clock, &from.shared.clock) {
            return Err(CacheError::ClockMismatch);
        }
        let mut entries = from.resident();
        entries.sort_unstable_by_key(|(access, _)| *access);
        let offered = entries.len();
        for (_, entry) in entries {
            if self.admits_resident(&entry) {
                self.insert_resident(&entry);
            }
        }
        Ok(Inherited {
            offered,
            kept: self.shared.usage.entries.load(Ordering::Relaxed),
        })
    }

    fn resident(&self) -> Vec<(u64, Arc<PreparedEntry>)> {
        let mut entries = Vec::with_capacity(self.shared.usage.entries.load(Ordering::Relaxed));
        for shard in &self.shared.shards {
            let state = shard.lock();
            entries.extend(
                state
                    .entries
                    .iter()
                    .map(|(_, stored)| (stored.last_access, Arc::clone(&stored.entry))),
            );
        }
        entries
    }

    fn admits_resident(&self, entry: &PreparedEntry) -> bool {
        let config = &self.shared.config;
        entry.charge <= config.max_object_bytes
            && entry.charge <= config.max_total_bytes
            && entry.body.len() <= config.max_body_bytes
            && entry.key.encoded_len() <= config.max_key_bytes
            && entry.key.vary.len() <= config.max_vary_fields
            && entry.tags.len() <= config.max_tags_per_entry
            && entry
                .tags
                .iter()
                .all(|tag| tag.len() <= config.max_tag_bytes)
            && check_header_bounds(&entry.headers, config).is_ok()
    }

    fn insert_resident(&self, entry: &PreparedEntry) {
        let entry = PreparedEntry {
            identity: Arc::clone(&self.shared.identity),
            ..entry.clone()
        };
        let mut state = self.shared.shard(entry.key.base()).lock();
        let mut delta = state.insert_entry(entry);
        drop(state);
        self.shared.evict_to_bounds(&mut delta, None);
    }
}

#[cfg(test)]
mod tests;
