//! One snapshot per second for every viewer: the page listener is open to anyone who can reach it, so
//! a request must never cost a rebuild of its own.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use arc_swap::ArcSwapOption;
use tokio::{sync::Mutex, time::Instant};

use super::{
    builder::BuildError,
    csv, html,
    model::{StatsDocument, StatsSnapshot},
};

/// A way to show the snapshot. Admin variants differ from the plain ones only for loopback peers, so
/// each of the five is rendered at most once per snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Rendering {
    Json { admin: bool },
    Csv,
    Html { admin: bool },
}

impl Rendering {
    const COUNT: usize = 5;

    const fn slot(self) -> usize {
        match self {
            Self::Json { admin } => admin as usize,
            Self::Csv => 2,
            Self::Html { admin } => 3 + admin as usize,
        }
    }
}

pub(super) struct Entry {
    built_at: Instant,
    pub(super) snapshot: StatsSnapshot,
    renderings: [OnceLock<Vec<u8>>; Rendering::COUNT],
}

impl Entry {
    fn new(snapshot: StatsSnapshot) -> Self {
        Self {
            built_at: Instant::now(),
            snapshot,
            renderings: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    /// `form_action` is the page prefix an admin table posts to; `refresh_ms` the no-script refresh.
    pub(super) fn body(&self, rendering: Rendering, refresh_ms: u64, form_action: &str) -> &[u8] {
        self.renderings[rendering.slot()].get_or_init(|| match rendering {
            Rendering::Json { admin } => serde_json::to_vec(&StatsDocument {
                admin,
                snapshot: &self.snapshot,
            })
            .expect("snapshot serialises"),
            Rendering::Csv => csv::render(&self.snapshot),
            Rendering::Html { admin } => {
                let revision = self.snapshot.generation.active_revision.as_deref();
                let form = revision
                    .filter(|_| admin)
                    .map(|revision| (form_action, revision));
                html::render(&self.snapshot, refresh_ms, form)
            }
        })
    }
}

pub(super) struct SnapshotCache {
    ttl: Duration,
    current: ArcSwapOption<Entry>,
    /// Held while one caller rebuilds; the others wait for its result instead of building their own.
    rebuilding: Mutex<()>,
}

impl SnapshotCache {
    pub(super) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            current: ArcSwapOption::empty(),
            rebuilding: Mutex::new(()),
        }
    }

    pub(super) async fn get(
        &self,
        build: impl FnOnce() -> Result<StatsSnapshot, BuildError>,
    ) -> Result<Arc<Entry>, BuildError> {
        if let Some(entry) = self.fresh() {
            return Ok(entry);
        }
        let _rebuilding = self.rebuilding.lock().await;
        if let Some(entry) = self.fresh() {
            return Ok(entry);
        }
        let entry = Arc::new(Entry::new(build()?));
        self.current.store(Some(Arc::clone(&entry)));
        Ok(entry)
    }

    /// Makes the next read rebuild, so a viewer sees the effect of their own change.
    pub(super) fn invalidate(&self) {
        self.current.store(None);
    }

    fn fresh(&self) -> Option<Arc<Entry>> {
        self.current
            .load_full()
            .filter(|entry| entry.built_at.elapsed() < self.ttl)
    }
}
