#[allow(clippy::wildcard_imports)]
use super::*;

/// RAII ownership of one publisher incarnation.
pub struct PublisherLease {
    pub(super) shared: Arc<LiveHubShared>,
    pub(super) key: StreamKey,
    pub(super) incarnation: PublisherIncarnation,
}

impl PublisherLease {
    #[must_use]
    pub fn key(&self) -> &StreamKey {
        &self.key
    }

    #[must_use]
    pub const fn incarnation(&self) -> PublisherIncarnation {
        self.incarnation
    }

    /// Fans an event into bounded viewer queues without performing I/O or waiting for capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when this incarnation is stale or a cacheable event exceeds its cache
    /// bound.
    pub fn publish(&self, event: MediaEvent) -> Result<PublishReport, LiveHubError> {
        validate_cache_size(self.shared.limits, &event)?;
        let mut state = self.shared.lock();
        let mut fanout_bytes = state.fanout_bytes;
        let stream = state
            .streams
            .get_mut(&self.key)
            .filter(|stream| stream.publisher == Some(self.incarnation))
            .ok_or_else(|| LiveHubError::PublisherExpired {
                key: self.key.clone(),
            })?;
        let video_transition = stream.cache.transition(&event);
        let mut subscriber_ids: Vec<_> = stream.subscribers.keys().copied().collect();
        subscriber_ids.sort_by_key(|subscriber_id| {
            let subscriber = &stream.subscribers[subscriber_id];
            (subscriber.bytes, *subscriber_id)
        });
        let mut report = PublishReport::default();
        for subscriber_id in subscriber_ids {
            if let Some(subscriber) = stream.subscribers.get_mut(&subscriber_id) {
                apply_video_transition(
                    subscriber,
                    video_transition,
                    &mut fanout_bytes,
                    &mut report,
                );
                fanout_event(
                    subscriber,
                    &stream.cache,
                    &event,
                    self.shared.limits,
                    &mut fanout_bytes,
                    &mut report,
                );
            }
        }
        report.stream_fanout_bytes = stream
            .subscribers
            .values()
            .map(|subscriber| subscriber.bytes)
            .sum();
        stream.cache.store(event);
        state.fanout_bytes = fanout_bytes;
        Ok(report)
    }
}

impl Drop for PublisherLease {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        let Some(stream) = state.streams.get_mut(&self.key) else {
            return;
        };
        if stream.publisher != Some(self.incarnation) {
            return;
        }

        stream.publisher = None;
        stream.cache = MediaCache::default();
        let released = reset_subscribers(stream);
        let remove_stream = stream.subscribers.is_empty();
        state.fanout_bytes -= released;
        if remove_stream {
            state.streams.remove(&self.key);
        }
    }
}

/// Pull-only handle for one bounded playback queue.
pub struct PlaybackSubscription {
    pub(super) shared: Arc<LiveHubShared>,
    pub(super) key: StreamKey,
    pub(super) subscriber_id: u64,
}

impl PlaybackSubscription {
    #[must_use]
    pub fn key(&self) -> &StreamKey {
        &self.key
    }

    /// Removes and returns the next queued event without performing I/O.
    #[must_use]
    pub fn try_next(&self) -> Option<MediaEvent> {
        let mut state = self.shared.lock();
        let (event, bytes) = {
            let subscriber = state
                .streams
                .get_mut(&self.key)?
                .subscribers
                .get_mut(&self.subscriber_id)?;
            let event = subscriber.events.pop_front()?;
            subscriber.bytes -= event.payload_len();
            let bytes = event.payload_len();
            (event, bytes)
        };
        state.fanout_bytes -= bytes;
        Some(event)
    }

    #[must_use]
    pub fn queued_messages(&self) -> usize {
        self.with_queue(|subscriber| subscriber.events.len())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        self.with_queue(|subscriber| subscriber.bytes).unwrap_or(0)
    }

    #[must_use]
    pub fn is_waiting_for_keyframe(&self) -> bool {
        self.with_queue(|subscriber| {
            matches!(
                subscriber.state,
                SubscriberState::AwaitingMedia | SubscriberState::AwaitingKeyframe
            )
        })
        .unwrap_or(true)
    }

    fn with_queue<T>(&self, inspect: impl FnOnce(&SubscriberQueue) -> T) -> Option<T> {
        let state = self.shared.lock();
        state
            .streams
            .get(&self.key)?
            .subscribers
            .get(&self.subscriber_id)
            .map(inspect)
    }
}

impl Drop for PlaybackSubscription {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        let Some(stream) = state.streams.get_mut(&self.key) else {
            return;
        };
        let Some(subscriber) = stream.subscribers.remove(&self.subscriber_id) else {
            return;
        };
        let remove_stream = stream.publisher.is_none() && stream.subscribers.is_empty();
        state.subscriber_count -= 1;
        state.fanout_bytes -= subscriber.bytes;
        if remove_stream {
            state.streams.remove(&self.key);
        }
    }
}

impl LiveHubShared {
    pub(super) fn lock(&self) -> MutexGuard<'_, LiveHubState> {
        self.state.lock().expect("live hub mutex poisoned")
    }
}
