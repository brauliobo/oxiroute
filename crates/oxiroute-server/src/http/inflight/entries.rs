use std::sync::Arc;

use super::{InflightArrival, InflightEntry, Place};

/// Entries a shard keeps for the next arrivals instead of freeing them: a worker serves one request after another.
const SPARE_ENTRIES: usize = 256;

/// The live entries of one shard, each in the slot it was admitted to: a slot is found by its number, and a
/// request that ends hands its slot to the next arrival.
#[derive(Default)]
pub(super) struct Entries {
    live: Vec<Option<Arc<InflightEntry>>>,
    vacant: Vec<usize>,
    spare: Vec<Arc<InflightEntry>>,
}

impl Entries {
    pub(super) fn admit(
        &mut self,
        shard: usize,
        arrival: InflightArrival<'_>,
        place: Place,
    ) -> Arc<InflightEntry> {
        let slot = self.vacant.pop().unwrap_or(self.live.len());
        let entry = match self.spare.pop() {
            Some(mut spare) => {
                Arc::get_mut(&mut spare)
                    .expect("a spare entry is held by no one else")
                    .renew((shard, slot), arrival, place);
                spare
            }
            None => Arc::new(InflightEntry::new((shard, slot), arrival, place)),
        };
        match self.live.get_mut(slot) {
            Some(vacated) => *vacated = Some(Arc::clone(&entry)),
            None => self.live.push(Some(Arc::clone(&entry))),
        }
        entry
    }

    /// Takes the entry out; it becomes a spare when no scan of the registry still holds it.
    pub(super) fn retire(&mut self, entry: &mut Arc<InflightEntry>) {
        let slot = entry.slot();
        self.live[slot] = None;
        self.vacant.push(slot);
        if self.spare.len() < SPARE_ENTRIES
            && let Some(unshared) = Arc::get_mut(entry)
        {
            unshared.release();
            self.spare.push(Arc::clone(entry));
        }
    }

    pub(super) fn live(&self) -> impl Iterator<Item = &Arc<InflightEntry>> {
        self.live.iter().flatten()
    }
}
