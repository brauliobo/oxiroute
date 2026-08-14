use std::fmt;

use crate::{RecorderWorkerConfig, RecordingPathPolicy, RecordingStore};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRecorderStart {
    Continuous,
    Manual,
}

#[derive(Clone)]
pub struct RtmpRecorderPolicy {
    name: String,
    start: RtmpRecorderStart,
    store: RecordingStore,
    path: RecordingPathPolicy,
    worker: RecorderWorkerConfig,
}

impl RtmpRecorderPolicy {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        start: RtmpRecorderStart,
        store: RecordingStore,
        path: RecordingPathPolicy,
        worker: RecorderWorkerConfig,
    ) -> Self {
        Self {
            name: name.into(),
            start,
            store,
            path,
            worker,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn start(&self) -> RtmpRecorderStart {
        self.start
    }

    #[must_use]
    pub const fn store(&self) -> &RecordingStore {
        &self.store
    }

    #[must_use]
    pub const fn path_policy(&self) -> &RecordingPathPolicy {
        &self.path
    }

    #[must_use]
    pub const fn worker_config(&self) -> RecorderWorkerConfig {
        self.worker
    }

    pub(crate) fn retire_store(&self) {
        self.store.retire();
    }
}

impl fmt::Debug for RtmpRecorderPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpRecorderPolicy")
            .field("name", &self.name)
            .field("start", &self.start)
            .field("path", &self.path)
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}
