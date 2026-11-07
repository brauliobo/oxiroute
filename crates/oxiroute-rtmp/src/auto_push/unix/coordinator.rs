#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub(crate) struct AutoPushCoordinator {
    shared: Arc<AutoPushShared>,
}

#[derive(Clone)]
pub(crate) struct AutoPushPublisher {
    shared: Arc<AutoPushShared>,
    key: StreamKey,
    token: SourceToken,
}

impl AutoPushCoordinator {
    pub(crate) fn new(
        config: RtmpAutoPushConfig,
        service_id: impl Into<Arc<str>>,
        registry: Arc<RtmpRegistry>,
        default_hub: LiveHub,
        application_hubs: BTreeMap<String, LiveHub>,
    ) -> Self {
        let service_id = service_id.into();
        let mut digest = Sha256::new();
        digest.update(service_id.as_bytes());
        let digest = digest.finalize();
        let mut service_hash = [0; 4];
        service_hash.copy_from_slice(&digest[..4]);
        let worker_id = *Uuid::new_v4().as_bytes();
        Self {
            shared: Arc::new(AutoPushShared {
                config,
                service_id,
                service_hash,
                worker_id,
                registry,
                default_hub,
                application_hubs,
                stop: AtomicBool::new(false),
                start_lock: Mutex::new(()),
                next_connection_id: AtomicU64::new(0),
                wake: (Mutex::new(false), Condvar::new()),
                secret: Mutex::new(None),
                handles: Mutex::new(Vec::new()),
                state: Mutex::new(AutoPushState::default()),
            }),
        }
    }

    pub(crate) fn source(
        &self,
        key: StreamKey,
        session_id: SessionId,
        incarnation: PublisherIncarnation,
    ) -> Result<Option<AutoPushPublisher>, RtmpAutoPushError> {
        if !self.shared.config.enabled {
            return Ok(None);
        }
        self.ensure_started()?;
        let token = SourceToken {
            worker_id: self.shared.worker_id,
            session_id: session_id.to_string(),
            incarnation: incarnation.value(),
        };
        let mut state = self.shared.lock_state();
        if state.closed {
            return Err(RtmpAutoPushError::AdmissionClosed);
        }
        if state.sources.len().saturating_add(state.remotes.len()) >= self.shared.config.max_streams
        {
            state.counters.last_failure = Some(RtmpAutoPushError::StreamLimit);
            return Err(RtmpAutoPushError::StreamLimit);
        }
        if state.sources.contains_key(&key) {
            return Err(RtmpAutoPushError::AdmissionClosed);
        }
        state.sources.insert(
            key.clone(),
            SourceState {
                token: token.clone(),
                cache: AutoPushCache::default(),
            },
        );
        Ok(Some(AutoPushPublisher {
            shared: Arc::clone(&self.shared),
            key,
            token,
        }))
    }

    pub(crate) fn ensure_started(&self) -> Result<(), RtmpAutoPushError> {
        if !self.shared.config.enabled {
            return Ok(());
        }
        let _start_lock = self
            .shared
            .start_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        {
            let state = self.shared.lock_state();
            if state.closed {
                return Err(RtmpAutoPushError::AdmissionClosed);
            }
            if state.started {
                return Ok(());
            }
        }
        let result = self.shared.start();
        if let Err(error) = result {
            let mut state = self.shared.lock_state();
            state.counters.last_failure = Some(error);
        }
        result
    }

    pub(crate) fn close(&self) {
        self.shared.close();
    }

    pub(crate) fn status(&self) -> RtmpAutoPushStatus {
        self.shared.status()
    }
}

impl Drop for AutoPushCoordinator {
    fn drop(&mut self) {
        self.shared.close();
    }
}

impl AutoPushPublisher {
    pub(crate) fn publish(&self, event: &MediaEvent, sequence: u64) {
        let frame = AutoPushFrame {
            application: self.key.application.clone(),
            stream_name: self.key.name.clone(),
            token: self.token.clone(),
            sequence,
            event: event.clone(),
        };
        self.shared.publish(&self.key, &self.token, &frame);
    }
}

impl Drop for AutoPushPublisher {
    fn drop(&mut self) {
        self.shared.remove_source(&self.key, &self.token);
    }
}
