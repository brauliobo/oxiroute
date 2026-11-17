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
        let shard = self.shared.shard(key.base());
        if !shard
            .lock()
            .charge_variant(key, variants, bytes, self.shared.config.max_object_bytes)
        {
            return false;
        }
        let mut delta = InsertionDelta {
            inserted: key.clone(),
            removed: Vec::new(),
            evicted: 0,
        };
        if self.shared.evict_to_bounds(&mut delta, Some(key)) {
            return true;
        }
        shard.lock().refund_variant(key, variants, bytes);
        false
    }
}

impl State {
    fn charge_variant(
        &mut self,
        key: &CacheKey,
        variants: &Variants,
        bytes: usize,
        object_limit: usize,
    ) -> bool {
        let Some(stored) = self.entries.get_mut(key) else {
            return false;
        };
        let object = stored.charge.saturating_add(bytes);
        if !stored.entry.variants.same_as(variants) || object > object_limit {
            return false;
        }
        stored.charge = object;
        self.usage.bytes.fetch_add(bytes, Ordering::Relaxed);
        true
    }

    fn refund_variant(&mut self, key: &CacheKey, variants: &Variants, bytes: usize) {
        let Some(stored) = self.entries.get_mut(key) else {
            return;
        };
        if stored.entry.variants.same_as(variants) {
            stored.charge = stored.charge.saturating_sub(bytes);
            self.usage.bytes.fetch_sub(bytes, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests;
