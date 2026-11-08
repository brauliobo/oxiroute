#[allow(clippy::wildcard_imports)]
use super::*;

/// The servers of one membership that may be chosen, published once per change of their state.
///
/// A request reads the published view instead of testing every server. The pool bumps its
/// selectable epoch after every change that can alter a server's eligibility (administrative
/// state, health, ejection, breaker transition); a time-gated state (ejection backoff, open
/// breaker) also bounds the view's life through [`Self::valid_until_unix_ms`]. A view built
/// while a writer is mid-update is tagged with the older epoch, so the writer's bump retires it.
#[derive(Debug)]
pub(super) struct SelectableView {
    epoch: u64,
    valid_until_unix_ms: u64,
    /// The selectable servers by position in the membership, ascending.
    pub(super) entries: Box<[SelectableEntry]>,
}

/// What selection needs to know about one selectable server without touching the server.
#[derive(Clone, Copy, Debug)]
pub(super) struct SelectableEntry {
    pub(super) index: u32,
}

impl SelectableView {
    /// A view no epoch matches, so the first reader of a membership builds the real one.
    pub(super) fn unbuilt() -> Self {
        Self {
            epoch: u64::MAX,
            valid_until_unix_ms: 0,
            entries: Box::default(),
        }
    }

    pub(super) fn build(endpoints: &[Arc<PoolEndpoint>], epoch: u64, now_unix_ms: u64) -> Self {
        let mut valid_until_unix_ms = u64::MAX;
        let entries = endpoints
            .iter()
            .enumerate()
            .filter(|(_, server)| {
                valid_until_unix_ms = valid_until_unix_ms.min(server.selectable_after(now_unix_ms));
                server.selectable_at(now_unix_ms)
            })
            .map(|(index, _)| SelectableEntry {
                index: u32::try_from(index).expect("pool size fits u32"),
            })
            .collect();
        Self {
            epoch,
            valid_until_unix_ms,
            entries,
        }
    }

    pub(super) fn is_current(&self, epoch: u64, now_unix_ms: u64) -> bool {
        self.epoch == epoch && now_unix_ms < self.valid_until_unix_ms
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The selectable positions in rotation order from `start`.
    pub(super) fn rotation(&self, start: usize) -> impl Iterator<Item = usize> + '_ {
        let at = self
            .entries
            .partition_point(|entry| (entry.index as usize) < start);
        self.entries[at..]
            .iter()
            .chain(&self.entries[..at])
            .map(|entry| entry.index as usize)
    }
}
