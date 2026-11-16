use std::future::Future;

use oxiroute_compress::Coding;
use tokio::sync::OnceCell;

#[allow(clippy::wildcard_imports)]
use super::*;

/// Content-coded bodies of one cached representation, each built at most once and charged to the cache bound.
#[derive(Clone, Debug, Default)]
pub struct Variants(Arc<[OnceCell<Option<Bytes>>; Coding::ALL.len()]>);

impl Variants {
    fn slot(&self, coding: Coding) -> &OnceCell<Option<Bytes>> {
        &self.0[coding.index()]
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Cache {
    /// Returns the stored body coded with `coding`, building it once from the plain body with `build` and charging
    /// it to the entry. `None` means the representation is served uncoded: `build` declined, or the bound has no room.
    pub async fn variant<F, Fut>(
        &self,
        response: &CachedResponse,
        coding: Coding,
        build: F,
    ) -> Option<Bytes>
    where
        F: FnOnce(Bytes) -> Fut,
        Fut: Future<Output = Option<Bytes>>,
    {
        let variants = &response.variants;
        variants
            .slot(coding)
            .get_or_init(|| async {
                let coded = build(response.body.clone()).await?;
                self.admit_variant(&response.key, variants, coded.len())
                    .then_some(coded)
            })
            .await
            .clone()
    }

    /// Stores a body coded at fill time with the entry that is not yet cached. `false` when it exceeds the object bound.
    pub fn attach_variant(&self, entry: &mut PreparedEntry, coding: Coding, body: Bytes) -> bool {
        let charge = entry.charge.saturating_add(body.len());
        let fits = charge <= self.shared.config.max_object_bytes
            && charge <= self.shared.config.max_total_bytes;
        if fits && entry.variants.slot(coding).set(Some(body)).is_ok() {
            entry.charge = charge;
            return true;
        }
        false
    }

    fn admit_variant(&self, key: &CacheKey, variants: &Variants, bytes: usize) -> bool {
        let mut state = self.shared.lock();
        let Some(stored) = state.entries.get(key) else {
            return false;
        };
        let object = stored.entry.charge.saturating_add(bytes);
        if !stored.entry.variants.same_as(variants) || object > self.shared.config.max_object_bytes
        {
            return false;
        }
        while state.bytes_used.saturating_add(bytes) > self.shared.config.max_total_bytes {
            let Some(lru) = state.entries.least_recently_used().cloned() else {
                return false;
            };
            if &lru == key {
                return false;
            }
            if let Some(old) = state.entries.remove(&lru) {
                state.bytes_used = state.bytes_used.saturating_sub(old.entry.charge);
                state.stats.evictions = state.stats.evictions.saturating_add(1);
            }
        }
        state.entries.add_charge(key, bytes);
        state.bytes_used = state.bytes_used.saturating_add(bytes);
        true
    }
}

#[cfg(test)]
mod tests;
