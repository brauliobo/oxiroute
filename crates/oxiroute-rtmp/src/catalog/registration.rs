#[allow(clippy::wildcard_imports)]
use super::*;

/// RAII ownership of one publisher entry in an [`RtmpRegistry`].
pub struct PublisherRegistration {
    pub(super) registry: Arc<RtmpRegistry>,
    pub(super) stream_id: StreamId,
    pub(super) session_id: SessionId,
    pub(super) recorder_ids: Vec<RecorderId>,
    pub(super) last_observed_at_unix_ms: u64,
    pub(super) active: bool,
}

pub(crate) struct PublisherShutdown {
    pub(super) recorder_controls: Vec<Arc<RecorderController>>,
    pub(super) relay_controls: Vec<Arc<RtmpRelayController>>,
}

impl PublisherShutdown {
    fn empty() -> Self {
        Self {
            recorder_controls: Vec::new(),
            relay_controls: Vec::new(),
        }
    }

    pub(crate) fn shutdown(self, at_unix_ms: u64) {
        for control in self.recorder_controls {
            control.deactivate(at_unix_ms);
        }
        for control in self.relay_controls {
            control.deactivate();
        }
    }
}

impl PublisherRegistration {
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    #[must_use]
    pub fn recorder_ids(&self) -> &[RecorderId] {
        &self.recorder_ids
    }

    pub fn observe_at(&mut self, at_unix_ms: u64) {
        self.last_observed_at_unix_ms = self.last_observed_at_unix_ms.max(at_unix_ms);
    }

    /// Explicitly releases the registration at the supplied catalog timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if this registration is no longer the catalog publisher.
    pub fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        self.release_deferred(at_unix_ms)?.shutdown(at_unix_ms);
        Ok(())
    }

    pub(crate) fn release_deferred(
        &mut self,
        at_unix_ms: u64,
    ) -> Result<PublisherShutdown, CatalogError> {
        if !self.active {
            return Ok(PublisherShutdown::empty());
        }
        let at_unix_ms = at_unix_ms.max(self.last_observed_at_unix_ms);
        let shutdown =
            self.registry
                .detach_publisher_deferred(self.stream_id, self.session_id, at_unix_ms)?;
        self.active = false;
        Ok(shutdown)
    }

    pub(crate) const fn last_observed_at_unix_ms(&self) -> u64 {
        self.last_observed_at_unix_ms
    }
}

impl Drop for PublisherRegistration {
    fn drop(&mut self) {
        if self.active {
            let _ = self.registry.detach_publisher(
                self.stream_id,
                self.session_id,
                self.last_observed_at_unix_ms,
            );
        }
    }
}

/// RAII ownership of one subscriber entry in an [`RtmpRegistry`].
pub struct SubscriberRegistration {
    pub(super) registry: Arc<RtmpRegistry>,
    pub(super) stream_id: StreamId,
    pub(super) session_id: SessionId,
    pub(super) last_observed_at_unix_ms: u64,
    pub(super) active: bool,
}

impl SubscriberRegistration {
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn observe_at(&mut self, at_unix_ms: u64) {
        self.last_observed_at_unix_ms = self.last_observed_at_unix_ms.max(at_unix_ms);
    }

    /// Explicitly releases the registration at the supplied catalog timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if this registration is no longer present in the catalog.
    pub fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        if !self.active {
            return Ok(());
        }
        self.registry
            .detach_subscriber(self.stream_id, self.session_id, at_unix_ms)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for SubscriberRegistration {
    fn drop(&mut self) {
        if self.active {
            let _ = self.registry.detach_subscriber(
                self.stream_id,
                self.session_id,
                self.last_observed_at_unix_ms,
            );
        }
    }
}
