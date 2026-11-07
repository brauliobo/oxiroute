#[allow(clippy::wildcard_imports)]
use super::*;

impl RtmpRegistry {
    #[must_use]
    pub fn new(capabilities: RtmpCapabilities) -> Self {
        Self {
            capabilities,
            inner: Mutex::new(RegistryInner {
                admission_open: true,
                revision: 0,
                as_of_unix_ms: 0,
                streams: HashMap::new(),
                streams_by_key: HashMap::new(),
                current: Arc::new(RtmpCatalogSnapshot {
                    revision: 0,
                    as_of_unix_ms: 0,
                    capabilities,
                    streams: Vec::new(),
                }),
                snapshot_dirty: false,
                work_stats: RtmpRegistryWorkStats::default(),
            }),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Arc<RtmpCatalogSnapshot> {
        let mut inner = self.lock();
        refresh_recorder_statuses(&mut inner);
        if inner
            .streams
            .values()
            .any(|stream| !stream.relays.is_empty())
        {
            inner.snapshot_dirty = true;
        }
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
        Arc::clone(&inner.current)
    }

    #[must_use]
    pub fn work_stats(&self) -> RtmpRegistryWorkStats {
        self.lock().work_stats
    }

    /// Returns bounded snapshots of currently registered RTMP client sessions.
    #[must_use]
    pub fn session_snapshots(&self) -> Vec<RtmpClientSnapshot> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut snapshots: Vec<_> = sessions
            .values()
            .map(|session| session.snapshot())
            .collect();
        snapshots.sort_by_key(|session| session.session_id);
        snapshots
    }

    /// Queues a target-checked disconnect for one live RTMP session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session disappeared, its revision changed, its role changed, or
    /// another incompatible control request is pending.
    pub fn request_session_control(
        &self,
        session_id: SessionId,
        action: RtmpSessionControlAction,
        expected_revision: u64,
    ) -> Result<RtmpSessionControlOutcome, RtmpSessionControlError> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .get(&session_id)
            .ok_or(RtmpSessionControlError::NotFound)?
            .request(action, expected_revision)
    }

    pub(crate) fn register_session(
        self: &Arc<Self>,
        session_id: SessionId,
        service_id: &str,
        peer_addr: Option<std::net::IpAddr>,
    ) -> Option<Arc<RtmpSessionControl>> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions.len() >= MAX_RTMP_SESSION_CONTROLS {
            return None;
        }
        let control = RtmpSessionControl::new(session_id, service_id, peer_addr);
        sessions.insert(session_id, Arc::clone(&control));
        Some(control)
    }

    pub(crate) fn unregister_session(&self, session_id: SessionId) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    pub(crate) fn create_recorder_reaper(
        self: &Arc<Self>,
        capacity: usize,
    ) -> (Arc<RecorderReaperOwner>, RecorderReaperHandle) {
        RecorderReaper::start(capacity, Arc::downgrade(self))
    }

    #[cfg(test)]
    pub(crate) fn close_admission(&self) {
        self.lock().admission_open = false;
    }

    /// Registers a publisher whose catalog entry is removed when the returned owner is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when another publisher already owns the stream.
    pub fn register_publisher(
        self: &Arc<Self>,
        key: StreamKey,
        session_id: SessionId,
        recorder_definitions: Vec<RecorderDefinition>,
        at_unix_ms: u64,
    ) -> Result<PublisherRegistration, CatalogError> {
        let stream_id = self.attach_publisher(key, session_id, recorder_definitions, at_unix_ms)?;
        Ok(PublisherRegistration {
            registry: Arc::clone(self),
            stream_id,
            session_id,
            recorder_ids: self.recorder_ids(stream_id, session_id)?,
            last_observed_at_unix_ms: at_unix_ms,
            active: true,
        })
    }

    pub(crate) fn register_managed_publisher(
        self: &Arc<Self>,
        key: StreamKey,
        session_id: SessionId,
        recorders: Vec<(RecorderDefinition, Arc<RecorderController>)>,
        relays: Vec<Arc<RtmpRelayController>>,
        at_unix_ms: u64,
    ) -> Result<PublisherRegistration, CatalogError> {
        let stream_id =
            self.attach_publisher_inner(key, session_id, recorders, relays, at_unix_ms)?;
        Ok(PublisherRegistration {
            registry: Arc::clone(self),
            stream_id,
            session_id,
            recorder_ids: self.recorder_ids(stream_id, session_id)?,
            last_observed_at_unix_ms: at_unix_ms,
            active: true,
        })
    }

    pub(crate) fn stale_publisher_owner(
        &self,
        key: &StreamKey,
        now_unix_ms: u64,
        threshold_unix_ms: u64,
    ) -> Option<PublisherOwner> {
        let inner = self.lock();
        let stream_id = inner.streams_by_key.get(key).copied()?;
        let stream = inner.streams.get(&stream_id)?;
        let publisher = stream.publisher?;
        if now_unix_ms.saturating_sub(stream.publisher_activity_at_unix_ms) < threshold_unix_ms {
            return None;
        }
        Some(PublisherOwner {
            stream_id,
            session_id: publisher.session_id,
        })
    }

    pub(crate) fn detach_expected_publisher(
        &self,
        owner: PublisherOwner,
        at_unix_ms: u64,
    ) -> Result<PublisherShutdown, CatalogError> {
        let mut inner = self.lock();
        detach_publisher_inner(
            &mut inner,
            owner.stream_id,
            owner.session_id,
            at_unix_ms,
            self.capabilities,
        )
    }

    /// Registers a subscriber whose catalog entry is removed when the returned owner is dropped.
    ///
    /// # Errors
    ///
    /// This operation currently has no expected error state.
    pub fn register_subscriber(
        self: &Arc<Self>,
        key: StreamKey,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<SubscriberRegistration, CatalogError> {
        let stream_id = self.attach_subscriber(key, session_id, at_unix_ms)?;
        Ok(SubscriberRegistration {
            registry: Arc::clone(self),
            stream_id,
            session_id,
            last_observed_at_unix_ms: at_unix_ms,
            active: true,
        })
    }

    /// Attaches a publisher to a logical stream.
    ///
    /// # Errors
    ///
    /// Returns an error when another publisher already owns the stream.
    pub fn attach_publisher(
        &self,
        key: StreamKey,
        session_id: SessionId,
        recorder_definitions: Vec<RecorderDefinition>,
        at_unix_ms: u64,
    ) -> Result<StreamId, CatalogError> {
        if !recorder_definitions.is_empty() {
            return Err(CatalogError::RecordingUnavailable);
        }
        self.attach_publisher_inner(
            key,
            session_id,
            recorder_definitions
                .into_iter()
                .map(|definition| (definition, None))
                .collect(),
            Vec::new(),
            at_unix_ms,
        )
    }

    pub(super) fn attach_publisher_inner<C>(
        &self,
        key: StreamKey,
        session_id: SessionId,
        recorder_definitions: Vec<(RecorderDefinition, C)>,
        relays: Vec<Arc<RtmpRelayController>>,
        at_unix_ms: u64,
    ) -> Result<StreamId, CatalogError>
    where
        C: IntoRecorderControl,
    {
        let mut inner = self.lock();
        if !inner.admission_open {
            return Err(CatalogError::AdmissionClosed);
        }
        let stream_id = ensure_stream(&mut inner, key, at_unix_ms);
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;

        if let Some(publisher) = stream.publisher {
            if publisher.session_id == session_id {
                return Ok(stream_id);
            }
            return Err(CatalogError::PublisherAlreadyAttached {
                stream_id,
                session_id: publisher.session_id,
            });
        }

        stream.publisher = Some(PublisherSnapshot {
            session_id,
            attached_at_unix_ms: at_unix_ms,
        });
        stream.publisher_activity_at_unix_ms = at_unix_ms;
        stream.media = MediaSnapshot::default();
        stream.media_sample_sequence = 0;
        stream.relays = relays
            .into_iter()
            .map(|control| MutableRelay {
                id: RelayId::new(),
                control,
            })
            .collect();
        let mut recorder_order = Vec::with_capacity(recorder_definitions.len());
        stream.recorders = recorder_definitions
            .into_iter()
            .map(|(definition, control)| {
                let id = RecorderId::new();
                recorder_order.push(id);
                (
                    id,
                    MutableRecorder {
                        id,
                        name: definition.name,
                        manual: definition.manual,
                        phase: RecorderPhase::Idle,
                        changed_at_unix_ms: at_unix_ms,
                        bytes_written: 0,
                        current_relative_name: None,
                        last_completed_relative_name: None,
                        recoverable_partial_name: None,
                        published_but_not_durable_relative_name: None,
                        queue_messages: 0,
                        queue_bytes: 0,
                        events_enqueued: 0,
                        events_processed: 0,
                        events_dropped: 0,
                        segments_started: 0,
                        segments_completed: 0,
                        discontinuities: 0,
                        last_notification: None,
                        worker_generation: 0,
                        control: control.into_recorder_control(),
                    },
                )
            })
            .collect();
        stream.recorder_order = recorder_order;
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, at_unix_ms);
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
        Ok(stream_id)
    }

    /// Publishes one absolute media statistics sample for the current publisher.
    ///
    /// Returns `false` when a delayed sample sequence has already been superseded.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing stream or stale publisher session.
    pub fn update_media_sample(
        &self,
        stream_id: StreamId,
        publisher_session_id: SessionId,
        sequence: u64,
        media: MediaSnapshot,
        at_unix_ms: u64,
    ) -> Result<bool, CatalogError> {
        let mut inner = self.lock();
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        if stream.publisher.map(|publisher| publisher.session_id) != Some(publisher_session_id) {
            return Err(CatalogError::PublisherMismatch {
                stream_id,
                session_id: publisher_session_id,
            });
        }
        stream.publisher_activity_at_unix_ms = stream.publisher_activity_at_unix_ms.max(at_unix_ms);
        if sequence <= stream.media_sample_sequence {
            return Ok(false);
        }

        stream.media_sample_sequence = sequence;
        stream.media = media;
        stream.revision = stream.revision.saturating_add(1);
        inner.work_stats.media_updates = inner.work_stats.media_updates.saturating_add(1);
        mark_mutation(&mut inner, at_unix_ms);
        Ok(true)
    }

    /// Attaches a subscriber to a logical stream.
    ///
    /// # Errors
    ///
    /// This operation currently has no expected error state.
    pub fn attach_subscriber(
        &self,
        key: StreamKey,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<StreamId, CatalogError> {
        let mut inner = self.lock();
        if !inner.admission_open {
            return Err(CatalogError::AdmissionClosed);
        }
        let stream_id = ensure_stream(&mut inner, key, at_unix_ms);
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        let count = stream.subscribers.entry(session_id).or_default();
        *count = count.saturating_add(1);
        stream.revision = stream.revision.saturating_add(1);
        mark_mutation(&mut inner, at_unix_ms);
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
        Ok(stream_id)
    }

    /// Detaches the current publisher.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing stream or stale publisher session.
    pub fn detach_publisher(
        &self,
        stream_id: StreamId,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<(), CatalogError> {
        self.detach_publisher_deferred(stream_id, session_id, at_unix_ms)?
            .shutdown(at_unix_ms);
        Ok(())
    }

    pub(super) fn detach_publisher_deferred(
        &self,
        stream_id: StreamId,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<PublisherShutdown, CatalogError> {
        let mut inner = self.lock();
        detach_publisher_inner(
            &mut inner,
            stream_id,
            session_id,
            at_unix_ms,
            self.capabilities,
        )
    }

    /// Detaches a subscriber.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing stream or stale subscriber session.
    pub fn detach_subscriber(
        &self,
        stream_id: StreamId,
        session_id: SessionId,
        at_unix_ms: u64,
    ) -> Result<(), CatalogError> {
        let mut inner = self.lock();
        let remove = {
            let stream = inner
                .streams
                .get_mut(&stream_id)
                .ok_or(CatalogError::StreamNotFound(stream_id))?;
            let remove_session = match stream.subscribers.get_mut(&session_id) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => {
                    return Err(CatalogError::SubscriberNotFound {
                        stream_id,
                        session_id,
                    });
                }
            };
            if remove_session {
                stream.subscribers.remove(&session_id);
            }
            stream.revision = stream.revision.saturating_add(1);
            stream.publisher.is_none() && stream.subscribers.is_empty()
        };
        if remove {
            remove_stream(&mut inner, stream_id);
        }
        mark_mutation(&mut inner, at_unix_ms);
        publish_snapshot_if_dirty(&mut inner, self.capabilities);
        Ok(())
    }
}

fn detach_publisher_inner(
    inner: &mut RegistryInner,
    stream_id: StreamId,
    session_id: SessionId,
    at_unix_ms: u64,
    capabilities: RtmpCapabilities,
) -> Result<PublisherShutdown, CatalogError> {
    let (remove, recorder_controls, relay_controls) = {
        let stream = inner
            .streams
            .get_mut(&stream_id)
            .ok_or(CatalogError::StreamNotFound(stream_id))?;
        if stream.publisher.map(|publisher| publisher.session_id) != Some(session_id) {
            return Err(CatalogError::PublisherMismatch {
                stream_id,
                session_id,
            });
        }
        stream.publisher = None;
        stream.publisher_activity_at_unix_ms = 0;
        stream.media = MediaSnapshot::default();
        stream.media_sample_sequence = 0;
        let recorder_controls = stream
            .recorders
            .values()
            .filter_map(|recorder| recorder.control.clone())
            .collect::<Vec<_>>();
        let relay_controls = stream
            .relays
            .drain(..)
            .map(|relay| relay.control)
            .collect::<Vec<_>>();
        stream.recorders.clear();
        stream.recorder_order.clear();
        stream.revision = stream.revision.saturating_add(1);
        (
            stream.subscribers.is_empty(),
            recorder_controls,
            relay_controls,
        )
    };
    if remove {
        remove_stream(inner, stream_id);
    }
    mark_mutation(inner, at_unix_ms);
    publish_snapshot_if_dirty(inner, capabilities);
    Ok(PublisherShutdown {
        recorder_controls,
        relay_controls,
    })
}

fn ensure_stream(inner: &mut RegistryInner, key: StreamKey, at_unix_ms: u64) -> StreamId {
    if let Some(stream_id) = inner.streams_by_key.get(&key) {
        return *stream_id;
    }

    let stream = MutableStream::new(key.clone(), at_unix_ms);
    let stream_id = stream.id;
    inner.streams_by_key.insert(key, stream_id);
    inner.streams.insert(stream_id, stream);
    stream_id
}

fn remove_stream(inner: &mut RegistryInner, stream_id: StreamId) {
    if let Some(stream) = inner.streams.remove(&stream_id) {
        inner.streams_by_key.remove(&stream.key);
    }
}

pub(super) fn mark_mutation(inner: &mut RegistryInner, at_unix_ms: u64) {
    inner.revision = inner.revision.saturating_add(1);
    inner.as_of_unix_ms = inner.as_of_unix_ms.max(at_unix_ms);
    inner.snapshot_dirty = true;
}

pub(super) fn publish_snapshot_if_dirty(inner: &mut RegistryInner, capabilities: RtmpCapabilities) {
    if !inner.snapshot_dirty {
        return;
    }
    let mut streams: Vec<_> = inner
        .streams
        .values()
        .map(MutableStream::snapshot)
        .collect();
    streams.sort_by(|left, right| left.key.cmp(&right.key).then(left.id.cmp(&right.id)));
    inner.work_stats.snapshot_rebuilds = inner.work_stats.snapshot_rebuilds.saturating_add(1);
    inner.work_stats.snapshot_streams_visited = inner
        .work_stats
        .snapshot_streams_visited
        .saturating_add(streams.len() as u64);
    inner.current = Arc::new(RtmpCatalogSnapshot {
        revision: inner.revision,
        as_of_unix_ms: inner.as_of_unix_ms,
        capabilities,
        streams,
    });
    inner.snapshot_dirty = false;
}
