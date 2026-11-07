use std::{collections::BTreeMap, sync::Arc};

use super::{RtmpAutoPushConfig, RtmpAutoPushError, RtmpAutoPushStatus};
use crate::{LiveHub, MediaEvent, PublisherIncarnation, RtmpRegistry, SessionId, StreamKey};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AutoPushCoordinator;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AutoPushPublisher;

impl AutoPushCoordinator {
    pub(crate) fn new(
        _config: RtmpAutoPushConfig,
        _service_id: impl Into<Arc<str>>,
        _registry: Arc<RtmpRegistry>,
        _default_hub: LiveHub,
        _application_hubs: BTreeMap<String, LiveHub>,
    ) -> Self {
        Self
    }

    pub(crate) fn source(
        &self,
        _key: StreamKey,
        _session_id: SessionId,
        _incarnation: PublisherIncarnation,
    ) -> Result<Option<AutoPushPublisher>, RtmpAutoPushError> {
        Err(RtmpAutoPushError::UnsupportedPlatform)
    }

    pub(crate) fn ensure_started(&self) -> Result<(), RtmpAutoPushError> {
        Err(RtmpAutoPushError::UnsupportedPlatform)
    }

    pub(crate) fn close(&self) {}

    pub(crate) fn status(&self) -> RtmpAutoPushStatus {
        RtmpAutoPushStatus {
            enabled: true,
            last_failure: Some(RtmpAutoPushError::UnsupportedPlatform),
            ..RtmpAutoPushStatus::default()
        }
    }
}

impl AutoPushPublisher {
    pub(crate) fn publish(&self, _event: &MediaEvent, _sequence: u64) {}
}
