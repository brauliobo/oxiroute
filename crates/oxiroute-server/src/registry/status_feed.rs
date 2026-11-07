//! Lets the registry's read-only socket serve `/status.json` for the generation being served.

use std::sync::{Arc, RwLock};

use serde_json::Value;

use super::service::RegistryService;
use crate::{GenerationManager, RuntimeMetrics, status_summary::status_document};

/// The newest generation's metrics and the generation manager, published at every activation.
pub(crate) struct StatusFeed {
    service: Arc<RegistryService>,
    latest: RwLock<Option<(RuntimeMetrics, GenerationManager)>>,
}

impl StatusFeed {
    pub(crate) fn new(service: Arc<RegistryService>) -> Self {
        Self {
            service,
            latest: RwLock::new(None),
        }
    }

    /// `generations` must not own the registry host that owns this feed.
    pub(crate) fn publish(&self, metrics: RuntimeMetrics, generations: GenerationManager) {
        *self
            .latest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((metrics, generations));
    }

    /// The status page, or `None` before the first generation is active.
    pub(crate) fn document(&self) -> Option<Value> {
        let latest = self
            .latest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        status_document(&latest.0, Some(&latest.1), Some(&self.service))
    }
}
