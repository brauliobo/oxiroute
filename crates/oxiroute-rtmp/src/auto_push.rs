use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

/// Runtime bounds for same-daemon RTMP worker auto-push.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpAutoPushConfig {
    pub enabled: bool,
    pub socket_dir: PathBuf,
    pub secret_file: Option<PathBuf>,
    pub reconnect_interval: Duration,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_peers: usize,
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub max_streams: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpAutoPushConfigError {
    #[error("auto-push field `{0}` is invalid")]
    InvalidField(&'static str),
}

impl RtmpAutoPushConfig {
    /// Validates value-only bounds and paths without reading credentials or creating sockets.
    ///
    /// # Errors
    ///
    /// Returns an error when an intrinsic path, duration, or queue/admission bound is invalid.
    pub fn validate_intrinsic(&self) -> Result<(), RtmpAutoPushConfigError> {
        if !valid_absolute_path(&self.socket_dir) {
            return Err(RtmpAutoPushConfigError::InvalidField("socket_dir"));
        }
        if self
            .secret_file
            .as_deref()
            .is_some_and(|path| !valid_absolute_path(path))
        {
            return Err(RtmpAutoPushConfigError::InvalidField("secret_file"));
        }
        if self.reconnect_interval.is_zero() {
            return Err(RtmpAutoPushConfigError::InvalidField("reconnect_interval"));
        }
        if self.connect_timeout.is_zero() {
            return Err(RtmpAutoPushConfigError::InvalidField("connect_timeout"));
        }
        if self.handshake_timeout.is_zero() {
            return Err(RtmpAutoPushConfigError::InvalidField("handshake_timeout"));
        }
        for (field, value) in [
            ("max_peers", self.max_peers),
            ("max_queue_messages", self.max_queue_messages),
            ("max_queue_bytes", self.max_queue_bytes),
            ("max_streams", self.max_streams),
        ] {
            if value == 0 {
                return Err(RtmpAutoPushConfigError::InvalidField(field));
            }
        }
        Ok(())
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 4_096
        && path.components().all(|component| {
            matches!(component, Component::RootDir | Component::Normal(_))
                && component.as_os_str().to_str().is_some_and(|value| {
                    !value
                        .bytes()
                        .any(|byte| byte == 0 || byte.is_ascii_control())
                })
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RtmpAutoPushError {
    #[error("RTMP auto-push is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("RTMP auto-push local transport is unavailable")]
    TransportUnavailable,
    #[error("RTMP auto-push local credentials are unavailable")]
    CredentialsUnavailable,
    #[error("RTMP auto-push admission is closed")]
    AdmissionClosed,
    #[error("RTMP auto-push stream limit reached")]
    StreamLimit,
    #[error("RTMP auto-push peer limit reached")]
    PeerLimit,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RtmpAutoPushStatus {
    pub enabled: bool,
    pub started: bool,
    pub peers: usize,
    pub source_streams: usize,
    pub remote_streams: usize,
    pub frames_sent: u64,
    pub frames_received: u64,
    pub frames_dropped: u64,
    pub authentication_failures: u64,
    pub reconnects: u64,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub last_failure: Option<RtmpAutoPushError>,
}

#[cfg(unix)]
mod wire;

#[cfg(unix)]
mod unix {
    use std::{
        collections::{BTreeMap, VecDeque},
        fs::{self, File, OpenOptions},
        io::{self, Read, Write},
        os::unix::{
            fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        path::{Path, PathBuf},
        sync::{
            Arc, Condvar, Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    };

    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::wire::{Reader, put_string};
    use super::{RtmpAutoPushConfig, RtmpAutoPushError, RtmpAutoPushStatus};
    use crate::{
        LiveHub, MediaEvent, MediaEventKind, PublisherIncarnation, PublisherRegistration,
        RtmpRegistry, SessionId, StreamKey, VideoCodec, clock::unix_time_ms,
        media_snapshot::MediaSnapshotAccumulator,
    };

    const PROTOCOL_MAGIC: [u8; 4] = *b"ORAP";
    const PROTOCOL_VERSION: u8 = 1;
    const PACKET_HELLO: u8 = 1;
    const PACKET_HELLO_ACK: u8 = 2;
    const PACKET_MEDIA: u8 = 3;
    const HELLO_NONCE_BYTES: usize = 16;
    const SECRET_BYTES: usize = 32;
    const MAX_SECRET_BYTES: usize = 128;
    const MAX_SERVICE_ID_BYTES: usize = 256;
    const MAX_SESSION_ID_BYTES: usize = 64;
    const MAX_STREAM_COMPONENT_BYTES: usize = 512;
    const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024 + 256;
    const SOCKET_MODE: u32 = 0o600;
    const DIRECTORY_MODE: u32 = 0o700;
    const SOCKET_PREFIX: &str = "oxir-";
    const SOCKET_SUFFIX: &str = ".sock";
    const SOCKET_WAIT: Duration = Duration::from_millis(100);
    const DISCOVERY_LIMIT_MULTIPLIER: usize = 2;

    type WorkerId = [u8; 16];

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

    struct AutoPushShared {
        config: RtmpAutoPushConfig,
        service_id: Arc<str>,
        service_hash: [u8; 4],
        worker_id: WorkerId,
        registry: Arc<RtmpRegistry>,
        default_hub: LiveHub,
        application_hubs: BTreeMap<String, LiveHub>,
        stop: AtomicBool,
        start_lock: Mutex<()>,
        next_connection_id: AtomicU64,
        wake: (Mutex<bool>, Condvar),
        secret: Mutex<Option<Arc<[u8]>>>,
        handles: Mutex<Vec<thread::JoinHandle<()>>>,
        state: Mutex<AutoPushState>,
    }

    #[derive(Default)]
    struct AutoPushState {
        started: bool,
        closed: bool,
        endpoint: Option<PathBuf>,
        sources: BTreeMap<StreamKey, SourceState>,
        remotes: BTreeMap<StreamKey, RemoteStream>,
        peers: BTreeMap<WorkerId, PeerConnection>,
        counters: Counters,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct Counters {
        frames_sent: u64,
        frames_received: u64,
        frames_dropped: u64,
        authentication_failures: u64,
        reconnects: u64,
        last_failure: Option<RtmpAutoPushError>,
    }

    #[derive(Clone)]
    struct SourceState {
        token: SourceToken,
        cache: AutoPushCache,
    }

    struct RemoteStream {
        token: SourceToken,
        peer: WorkerId,
        session_id: SessionId,
        stream_id: crate::StreamId,
        lease: crate::PublisherLease,
        registration: PublisherRegistration,
        sequence: u64,
        media: MediaSnapshotAccumulator,
        hub: LiveHub,
        registry: Arc<RtmpRegistry>,
    }

    #[derive(Clone)]
    struct PeerConnection {
        id: u64,
        queue: Option<Arc<PeerQueue>>,
    }

    struct PeerQueue {
        max_messages: usize,
        max_bytes: usize,
        state: Mutex<PeerQueueState>,
        wake: Condvar,
    }

    struct PeerQueueState {
        accepting: bool,
        waiting_for_bootstrap: bool,
        queue: VecDeque<AutoPushFrame>,
        bytes: usize,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct SourceToken {
        worker_id: WorkerId,
        session_id: String,
        incarnation: u64,
    }

    #[derive(Clone)]
    struct AutoPushFrame {
        application: String,
        stream_name: String,
        token: SourceToken,
        sequence: u64,
        event: MediaEvent,
    }

    #[derive(Clone, Default)]
    struct AutoPushCache {
        metadata: Option<AutoPushFrame>,
        aac_header: Option<AutoPushFrame>,
        video_headers: BTreeMap<VideoCodec, AutoPushFrame>,
        keyframe: Option<AutoPushFrame>,
        latest_audio: Option<AutoPushFrame>,
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
            if state.sources.len().saturating_add(state.remotes.len())
                >= self.shared.config.max_streams
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

    impl PeerQueue {
        fn new(config: &RtmpAutoPushConfig) -> Arc<Self> {
            Arc::new(Self {
                max_messages: config.max_queue_messages,
                max_bytes: config.max_queue_bytes,
                state: Mutex::new(PeerQueueState {
                    accepting: true,
                    waiting_for_bootstrap: false,
                    queue: VecDeque::new(),
                    bytes: 0,
                }),
                wake: Condvar::new(),
            })
        }

        fn enqueue(&self, frames: &[AutoPushFrame], bootstrap: bool) -> bool {
            let mut state = self.lock();
            if !state.accepting || (state.waiting_for_bootstrap && !bootstrap) {
                return false;
            }
            let frame_bytes = frames
                .iter()
                .try_fold(0_usize, |total, frame| total.checked_add(frame.size()));
            let Some(frame_bytes) = frame_bytes else {
                state.waiting_for_bootstrap = true;
                Self::clear(&mut state);
                return false;
            };
            let Some(message_count) = state.queue.len().checked_add(frames.len()) else {
                state.waiting_for_bootstrap = true;
                Self::clear(&mut state);
                return false;
            };
            let Some(queue_bytes) = state.bytes.checked_add(frame_bytes) else {
                state.waiting_for_bootstrap = true;
                Self::clear(&mut state);
                return false;
            };
            if message_count > self.max_messages || queue_bytes > self.max_bytes {
                Self::clear(&mut state);
                state.waiting_for_bootstrap = true;
                if !bootstrap {
                    return false;
                }
                if frames.len() > self.max_messages || frame_bytes > self.max_bytes {
                    return false;
                }
            }
            state.queue.extend(frames.iter().cloned());
            state.bytes = state.bytes.saturating_add(frame_bytes);
            state.waiting_for_bootstrap = false;
            drop(state);
            self.wake.notify_one();
            true
        }

        fn next(&self) -> Option<AutoPushFrame> {
            let mut state = self.lock();
            while state.accepting && state.queue.is_empty() {
                state = self
                    .wake
                    .wait_timeout(state, super::unix::SOCKET_WAIT)
                    .expect("RTMP auto-push queue mutex poisoned")
                    .0;
            }
            let frame = state.queue.pop_front()?;
            state.bytes = state.bytes.saturating_sub(frame.size());
            Some(frame)
        }

        fn close(&self) {
            let mut state = self.lock();
            state.accepting = false;
            Self::clear(&mut state);
            drop(state);
            self.wake.notify_all();
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, PeerQueueState> {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn clear(state: &mut PeerQueueState) {
            state.queue.clear();
            state.bytes = 0;
        }
    }

    impl AutoPushFrame {
        fn size(&self) -> usize {
            self.application.len()
                + self.stream_name.len()
                + self.token.session_id.len()
                + self.event.payload_len()
                + 64
        }

        fn is_keyframe(&self) -> bool {
            self.event.kind() == MediaEventKind::VideoKeyframe
        }
    }

    impl AutoPushCache {
        fn update(&mut self, frame: AutoPushFrame) {
            match frame.event.kind() {
                MediaEventKind::Metadata => self.metadata = Some(frame),
                MediaEventKind::AacSequenceHeader => self.aac_header = Some(frame),
                MediaEventKind::AvcSequenceHeader
                | MediaEventKind::HevcSequenceHeader
                | MediaEventKind::Av1SequenceHeader => {
                    if let Some(codec) = frame.event.video_codec() {
                        self.keyframe = None;
                        self.video_headers.insert(codec, frame);
                    }
                }
                MediaEventKind::VideoKeyframe => self.keyframe = Some(frame),
                MediaEventKind::Audio => self.latest_audio = Some(frame),
                MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {}
            }
        }

        fn bootstrap(&self) -> Vec<AutoPushFrame> {
            let mut frames = Vec::with_capacity(4);
            frames.extend(self.metadata.iter().cloned());
            frames.extend(self.aac_header.iter().cloned());
            if let Some(keyframe) = &self.keyframe {
                if let Some(codec) = keyframe.event.video_codec() {
                    frames.extend(self.video_headers.get(&codec).cloned());
                }
                frames.push(keyframe.clone());
            } else {
                frames.extend(self.latest_audio.iter().cloned());
            }
            frames.sort_by_key(|frame| frame.sequence);
            frames
        }
    }

    impl AutoPushShared {
        fn lock_state(&self) -> std::sync::MutexGuard<'_, AutoPushState> {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn start(self: &Arc<Self>) -> Result<(), RtmpAutoPushError> {
            secure_directory(&self.config.socket_dir)?;
            let default_secret_path = self.config.socket_dir.join(".secret");
            let secret_path = self
                .config
                .secret_file
                .as_deref()
                .unwrap_or(&default_secret_path);
            let secret = load_secret(secret_path, self.config.secret_file.is_none())?;
            let endpoint =
                endpoint_path(&self.config.socket_dir, self.service_hash, self.worker_id)?;
            let listener = UnixListener::bind(&endpoint)
                .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
            if fs::set_permissions(&endpoint, fs::Permissions::from_mode(SOCKET_MODE)).is_err()
                || listener.set_nonblocking(true).is_err()
            {
                drop(listener);
                let _ = fs::remove_file(&endpoint);
                return Err(RtmpAutoPushError::TransportUnavailable);
            }
            *self
                .secret
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::from(secret));
            let accept_shared = Arc::clone(self);
            let discover_shared = Arc::clone(&accept_shared);
            let accept = thread::Builder::new()
                .name("rtmp-auto-push-accept".into())
                .spawn(move || accept_loop(&listener, &accept_shared))
                .map_err(|_| {
                    let _ = fs::remove_file(&endpoint);
                    RtmpAutoPushError::TransportUnavailable
                })?;
            let Ok(discover) = thread::Builder::new()
                .name("rtmp-auto-push-discover".into())
                .spawn(move || discovery_loop(&discover_shared))
            else {
                self.stop.store(true, Ordering::Release);
                self.wake.1.notify_all();
                let _ = accept.join();
                self.stop.store(false, Ordering::Release);
                let _ = fs::remove_file(&endpoint);
                return Err(RtmpAutoPushError::TransportUnavailable);
            };
            let mut state = self.lock_state();
            if state.closed || self.stop.load(Ordering::Acquire) {
                drop(state);
                self.wake.1.notify_all();
                let _ = accept.join();
                let _ = discover.join();
                let _ = fs::remove_file(&endpoint);
                return Err(RtmpAutoPushError::AdmissionClosed);
            }
            state.endpoint = Some(endpoint);
            state.started = true;
            self.handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend([accept, discover]);
            Ok(())
        }

        fn publish(&self, key: &StreamKey, token: &SourceToken, frame: &AutoPushFrame) {
            let bootstrap = {
                let mut state = self.lock_state();
                let Some(source) = state.sources.get_mut(key) else {
                    return;
                };
                if source.token != *token {
                    return;
                }
                source.cache.update(frame.clone());
                if frame.is_keyframe() {
                    Some(source.cache.bootstrap())
                } else {
                    None
                }
            };
            let peers: Vec<_> = self
                .lock_state()
                .peers
                .values()
                .filter_map(|peer| peer.queue.clone())
                .collect();
            for peer in peers {
                let accepted = bootstrap.as_ref().map_or_else(
                    || peer.enqueue(std::slice::from_ref(frame), false),
                    |frames| peer.enqueue(frames, true),
                );
                if !accepted {
                    let mut state = self.lock_state();
                    state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                }
            }
        }

        fn remove_source(&self, key: &StreamKey, token: &SourceToken) {
            let mut state = self.lock_state();
            if state
                .sources
                .get(key)
                .is_some_and(|source| source.token == *token)
            {
                state.sources.remove(key);
            }
        }

        fn receive(&self, peer: WorkerId, frame: &AutoPushFrame) {
            if frame.token.worker_id == self.worker_id {
                return;
            }
            let key = StreamKey::new(
                self.service_id.as_ref(),
                frame.application.clone(),
                frame.stream_name.clone(),
            );
            let mut state = self.lock_state();
            state.counters.frames_received = state.counters.frames_received.saturating_add(1);
            if state.sources.contains_key(&key) {
                state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                return;
            }
            let replace = state.remotes.get(&key).is_some_and(|remote| {
                remote.token.worker_id == frame.token.worker_id
                    && frame.token.incarnation > remote.token.incarnation
            });
            if let Some(remote) = state.remotes.get_mut(&key)
                && !replace
            {
                if remote.token != frame.token || frame.sequence <= remote.sequence {
                    state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                    return;
                }
                let at_unix_ms = unix_time_ms();
                if apply_remote_frame(remote, frame, at_unix_ms).is_err() {
                    state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                }
                return;
            }
            if replace && let Some(remote) = state.remotes.remove(&key) {
                remote.shutdown(unix_time_ms());
            }
            if state.sources.len().saturating_add(state.remotes.len()) >= self.config.max_streams {
                state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                state.counters.last_failure = Some(RtmpAutoPushError::StreamLimit);
                return;
            }
            let hub = self
                .application_hubs
                .get(&key.application)
                .cloned()
                .unwrap_or_else(|| self.default_hub.clone());
            let session_id = SessionId::new();
            let now = unix_time_ms();
            let (lease, registration) = {
                let transaction_hub = hub.clone();
                let _transaction = transaction_hub.lock_roles();
                let Ok(lease) = hub.attach_publisher(key.clone()) else {
                    state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                    return;
                };
                let Ok(registration) =
                    self.registry
                        .register_publisher(key.clone(), session_id, Vec::new(), now)
                else {
                    drop(lease);
                    state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                    return;
                };
                (lease, registration)
            };
            let stream_id = registration.stream_id();
            let mut remote = RemoteStream {
                token: frame.token.clone(),
                peer,
                session_id,
                stream_id,
                lease,
                registration,
                sequence: 0,
                media: MediaSnapshotAccumulator::default(),
                hub,
                registry: Arc::clone(&self.registry),
            };
            if apply_remote_frame(&mut remote, frame, now).is_err() {
                remote.shutdown(now);
                state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                return;
            }
            state.remotes.insert(key, remote);
        }

        fn add_peer(&self, worker_id: WorkerId, queue: Option<Arc<PeerQueue>>) -> Option<u64> {
            let mut state = self.lock_state();
            if state.closed || state.peers.contains_key(&worker_id) {
                return None;
            }
            if state.peers.len() >= self.config.max_peers {
                state.counters.last_failure = Some(RtmpAutoPushError::PeerLimit);
                return None;
            }
            let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
            state.peers.insert(worker_id, PeerConnection { id, queue });
            Some(id)
        }

        fn remove_peer(&self, worker_id: WorkerId, id: u64) {
            let mut state = self.lock_state();
            if state
                .peers
                .get(&worker_id)
                .is_some_and(|peer| peer.id == id)
            {
                if let Some(peer) = state.peers.remove(&worker_id)
                    && let Some(queue) = peer.queue
                {
                    queue.close();
                }
                let keys: Vec<_> = state
                    .remotes
                    .iter()
                    .filter_map(|(key, remote)| (remote.peer == worker_id).then_some(key.clone()))
                    .collect();
                for key in keys {
                    if let Some(remote) = state.remotes.remove(&key) {
                        remote.shutdown(unix_time_ms());
                    }
                }
                state.counters.reconnects = state.counters.reconnects.saturating_add(1);
            }
        }

        fn sync_peer(&self, queue: &PeerQueue) {
            let batches: Vec<_> = self
                .lock_state()
                .sources
                .values()
                .map(|source| source.cache.bootstrap())
                .filter(|frames| !frames.is_empty())
                .collect();
            for batch in batches {
                let _ = queue.enqueue(&batch, true);
            }
        }

        fn close(&self) {
            if self.stop.swap(true, Ordering::AcqRel) {
                return;
            }
            self.wake.1.notify_all();
            let peers: Vec<_> = self
                .lock_state()
                .peers
                .values()
                .filter_map(|peer| peer.queue.clone())
                .collect();
            for peer in peers {
                peer.close();
            }
            let endpoint = {
                let mut state = self.lock_state();
                state.closed = true;
                state.sources.clear();
                for (_, remote) in std::mem::take(&mut state.remotes) {
                    remote.shutdown(unix_time_ms());
                }
                state.peers.clear();
                state.endpoint.take()
            };
            if let Some(endpoint) = endpoint {
                let _ = fs::remove_file(endpoint);
            }
            let handles = std::mem::take(
                &mut *self
                    .handles
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            for handle in handles {
                let _ = handle.join();
            }
        }

        fn status(&self) -> RtmpAutoPushStatus {
            let state = self.lock_state();
            let (queue_messages, queue_bytes) = state
                .peers
                .values()
                .filter_map(|peer| peer.queue.as_ref())
                .map(|queue| {
                    let state = queue.lock();
                    (state.queue.len(), state.bytes)
                })
                .fold(
                    (0_usize, 0_usize),
                    |(messages, bytes), (next_messages, next_bytes)| {
                        (
                            messages.saturating_add(next_messages),
                            bytes.saturating_add(next_bytes),
                        )
                    },
                );
            RtmpAutoPushStatus {
                enabled: self.config.enabled,
                started: state.started,
                peers: state.peers.len(),
                source_streams: state.sources.len(),
                remote_streams: state.remotes.len(),
                frames_sent: state.counters.frames_sent,
                frames_received: state.counters.frames_received,
                frames_dropped: state.counters.frames_dropped,
                authentication_failures: state.counters.authentication_failures,
                reconnects: state.counters.reconnects,
                queue_messages,
                queue_bytes,
                last_failure: state.counters.last_failure,
            }
        }
    }

    impl RemoteStream {
        fn shutdown(mut self, at_unix_ms: u64) {
            let _transaction = self.hub.lock_roles();
            let _ = self.registration.release(at_unix_ms);
            drop(self.lease);
        }
    }

    fn apply_remote_frame(
        remote: &mut RemoteStream,
        frame: &AutoPushFrame,
        at_unix_ms: u64,
    ) -> Result<(), ()> {
        if frame.sequence <= remote.sequence || frame.token != remote.token {
            return Err(());
        }
        remote.lease.publish(frame.event.clone()).map_err(|_| ())?;
        remote.sequence = frame.sequence;
        remote.media.observe(&frame.event, at_unix_ms);
        remote.registration.observe_at(at_unix_ms);
        remote
            .registry
            .update_media_sample(
                remote.stream_id,
                remote.session_id,
                remote.sequence,
                remote.media.snapshot(0),
                at_unix_ms,
            )
            .map_err(|_| ())?;
        Ok(())
    }

    fn accept_loop(listener: &UnixListener, shared: &Arc<AutoPushShared>) {
        while !shared.stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let shared = Arc::clone(shared);
                    let _ = thread::Builder::new()
                        .name("rtmp-auto-push-peer".into())
                        .spawn(move || handle_incoming(stream, &shared));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    wait_for_wakeup(shared, SOCKET_WAIT);
                }
                Err(_) => {
                    wait_for_wakeup(shared, SOCKET_WAIT);
                }
            }
        }
    }

    fn discovery_loop(shared: &Arc<AutoPushShared>) {
        while !shared.stop.load(Ordering::Acquire) {
            discover_peers(shared);
            wait_for_wakeup(shared, shared.config.reconnect_interval);
        }
    }

    fn discover_peers(shared: &Arc<AutoPushShared>) {
        let Ok(entries) = fs::read_dir(&shared.config.socket_dir) else {
            return;
        };
        let mut endpoints = Vec::new();
        for entry in entries.flatten().take(
            shared
                .config
                .max_peers
                .saturating_mul(DISCOVERY_LIMIT_MULTIPLIER)
                .saturating_add(1),
        ) {
            let path = entry.path();
            let Some(worker_id) = parse_endpoint(&path, shared.service_hash) else {
                continue;
            };
            if worker_id <= shared.worker_id || worker_id == shared.worker_id {
                continue;
            }
            endpoints.push((worker_id, path));
        }
        endpoints.sort_by_key(|(worker_id, _)| *worker_id);
        for (worker_id, endpoint) in endpoints {
            if shared.stop.load(Ordering::Acquire)
                || shared.lock_state().peers.contains_key(&worker_id)
            {
                continue;
            }
            connect_outgoing(shared, worker_id, &endpoint);
        }
    }

    fn connect_outgoing(shared: &Arc<AutoPushShared>, expected_worker: WorkerId, endpoint: &Path) {
        let Ok(mut stream) = UnixStream::connect(endpoint) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(shared.config.connect_timeout));
        let _ = stream.set_write_timeout(Some(shared.config.connect_timeout));
        if handshake_client(&mut stream, shared, expected_worker).is_err() {
            return;
        }
        let queue = PeerQueue::new(&shared.config);
        let Some(id) = shared.add_peer(expected_worker, Some(Arc::clone(&queue))) else {
            return;
        };
        shared.sync_peer(&queue);
        let Ok(reader_stream) = stream.try_clone() else {
            shared.remove_peer(expected_worker, id);
            return;
        };
        let writer_shared = Arc::clone(shared);
        let _ = thread::Builder::new()
            .name("rtmp-auto-push-writer".into())
            .spawn(move || writer_loop(stream, &queue, &writer_shared, expected_worker, id));
        let reader_shared = Arc::clone(shared);
        let _ = thread::Builder::new()
            .name("rtmp-auto-push-reader".into())
            .spawn(move || receive_loop(reader_stream, &reader_shared, expected_worker, id));
    }

    fn writer_loop(
        mut stream: UnixStream,
        queue: &Arc<PeerQueue>,
        shared: &Arc<AutoPushShared>,
        worker_id: WorkerId,
        connection_id: u64,
    ) {
        let timeout = Some(SOCKET_WAIT);
        let _ = stream.set_write_timeout(timeout);
        while !shared.stop.load(Ordering::Acquire) {
            let Some(frame) = queue.next() else {
                break;
            };
            let Ok(encoded) = encode_media(&shared.service_id, &frame) else {
                break;
            };
            if write_packet(&mut stream, PACKET_MEDIA, &encoded).is_err() {
                break;
            }
            let mut state = shared.lock_state();
            state.counters.frames_sent = state.counters.frames_sent.saturating_add(1);
        }
        shared.remove_peer(worker_id, connection_id);
    }

    fn handle_incoming(mut stream: UnixStream, shared: &Arc<AutoPushShared>) {
        let timeout = Some(shared.config.handshake_timeout);
        let _ = stream.set_read_timeout(timeout);
        let _ = stream.set_write_timeout(timeout);
        let Ok(worker_id) = handshake_server(&mut stream, shared) else {
            let mut state = shared.lock_state();
            state.counters.authentication_failures =
                state.counters.authentication_failures.saturating_add(1);
            return;
        };
        let Ok(writer_stream) = stream.try_clone() else {
            return;
        };
        let queue = PeerQueue::new(&shared.config);
        let Some(connection_id) = shared.add_peer(worker_id, Some(Arc::clone(&queue))) else {
            return;
        };
        shared.sync_peer(&queue);
        let writer_shared = Arc::clone(shared);
        let _ = thread::Builder::new()
            .name("rtmp-auto-push-writer".into())
            .spawn(move || {
                writer_loop(
                    writer_stream,
                    &queue,
                    &writer_shared,
                    worker_id,
                    connection_id,
                );
            });
        receive_loop(stream, shared, worker_id, connection_id);
    }

    fn receive_loop(
        mut stream: UnixStream,
        shared: &Arc<AutoPushShared>,
        worker_id: WorkerId,
        connection_id: u64,
    ) {
        let _ = stream.set_read_timeout(Some(SOCKET_WAIT));
        loop {
            if shared.stop.load(Ordering::Acquire) {
                break;
            }
            let packet = match read_packet(&mut stream) {
                Ok(packet) => packet,
                Err(PacketReadError::Timeout) => continue,
                Err(_) => break,
            };
            if packet.0 != PACKET_MEDIA {
                break;
            }
            if let Ok(frame) = decode_media(&shared.service_id, &packet.1) {
                shared.receive(worker_id, &frame);
            } else {
                let mut state = shared.lock_state();
                state.counters.frames_dropped = state.counters.frames_dropped.saturating_add(1);
                break;
            }
        }
        shared.remove_peer(worker_id, connection_id);
    }

    fn handshake_client(
        stream: &mut UnixStream,
        shared: &AutoPushShared,
        expected_worker: WorkerId,
    ) -> Result<(), RtmpAutoPushError> {
        let nonce = *Uuid::new_v4().as_bytes();
        let secret = shared
            .secret
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(RtmpAutoPushError::CredentialsUnavailable)?;
        let body = hello_body(
            &shared.service_id,
            shared.worker_id,
            nonce,
            &secret,
            b"hello",
        )?;
        write_packet(stream, PACKET_HELLO, &body)
            .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        let (kind, body) =
            read_packet(stream).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        if kind != PACKET_HELLO_ACK {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        verify_ack(
            &body,
            &shared.service_id,
            expected_worker,
            shared.worker_id,
            nonce,
            &secret,
        )
    }

    fn handshake_server(
        stream: &mut UnixStream,
        shared: &AutoPushShared,
    ) -> Result<WorkerId, RtmpAutoPushError> {
        let (kind, body) =
            read_packet(stream).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        if kind != PACKET_HELLO {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        let (service_id, worker_id, nonce, proof) = parse_hello(&body)?;
        if service_id != shared.service_id.as_ref() || worker_id == shared.worker_id {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        let secret = shared
            .secret
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(RtmpAutoPushError::CredentialsUnavailable)?;
        let expected = auth_digest(
            &secret,
            b"hello",
            service_id.as_bytes(),
            &worker_id,
            &nonce,
            &[],
        );
        if !constant_time_eq(&expected, &proof) {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        let body = ack_body(
            &shared.service_id,
            shared.worker_id,
            worker_id,
            nonce,
            &secret,
        )?;
        write_packet(stream, PACKET_HELLO_ACK, &body)
            .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        Ok(worker_id)
    }

    fn hello_body(
        service_id: &str,
        worker_id: WorkerId,
        nonce: [u8; HELLO_NONCE_BYTES],
        secret: &[u8],
        label: &[u8],
    ) -> Result<Vec<u8>, RtmpAutoPushError> {
        let mut body = Vec::with_capacity(2 + service_id.len() + 16 + 16 + 32);
        put_string(&mut body, service_id, MAX_SERVICE_ID_BYTES)?;
        body.extend_from_slice(&worker_id);
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&auth_digest(
            secret,
            label,
            service_id.as_bytes(),
            &worker_id,
            &nonce,
            &[],
        ));
        Ok(body)
    }

    fn ack_body(
        service_id: &str,
        server_worker: WorkerId,
        client_worker: WorkerId,
        nonce: [u8; HELLO_NONCE_BYTES],
        secret: &[u8],
    ) -> Result<Vec<u8>, RtmpAutoPushError> {
        let mut body = Vec::with_capacity(2 + service_id.len() + 16 + 16 + 16 + 32);
        put_string(&mut body, service_id, MAX_SERVICE_ID_BYTES)?;
        body.extend_from_slice(&server_worker);
        body.extend_from_slice(&client_worker);
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&auth_digest(
            secret,
            b"ack",
            service_id.as_bytes(),
            &server_worker,
            &nonce,
            &client_worker,
        ));
        Ok(body)
    }

    fn parse_hello(
        body: &[u8],
    ) -> Result<(String, WorkerId, [u8; 16], [u8; 32]), RtmpAutoPushError> {
        let mut reader = Reader::new(body);
        let service_id = reader.string(MAX_SERVICE_ID_BYTES)?;
        let worker_id = reader.array::<16>()?;
        let nonce = reader.array::<16>()?;
        let proof = reader.array::<32>()?;
        reader.finish()?;
        Ok((service_id, worker_id, nonce, proof))
    }

    fn verify_ack(
        body: &[u8],
        service_id: &str,
        expected_server: WorkerId,
        client_worker: WorkerId,
        nonce: [u8; 16],
        secret: &[u8],
    ) -> Result<(), RtmpAutoPushError> {
        let mut reader = Reader::new(body);
        let received_service = reader.string(MAX_SERVICE_ID_BYTES)?;
        let server_worker = reader.array::<16>()?;
        let received_client = reader.array::<16>()?;
        let received_nonce = reader.array::<16>()?;
        let proof = reader.array::<32>()?;
        reader.finish()?;
        let expected = auth_digest(
            secret,
            b"ack",
            service_id.as_bytes(),
            &server_worker,
            &received_nonce,
            &received_client,
        );
        if received_service != service_id
            || server_worker != expected_server
            || received_client != client_worker
            || received_nonce != nonce
            || !constant_time_eq(&expected, &proof)
        {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        Ok(())
    }

    fn encode_media(service_id: &str, frame: &AutoPushFrame) -> Result<Vec<u8>, RtmpAutoPushError> {
        if frame.application.len() > MAX_STREAM_COMPONENT_BYTES
            || frame.stream_name.len() > MAX_STREAM_COMPONENT_BYTES
            || frame.token.session_id.len() > MAX_SESSION_ID_BYTES
            || frame.event.payload_len() > MAX_PACKET_BYTES
        {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        let mut body = Vec::with_capacity(frame.size());
        put_string(&mut body, service_id, MAX_SERVICE_ID_BYTES)?;
        body.extend_from_slice(&frame.token.worker_id);
        put_string(&mut body, &frame.token.session_id, MAX_SESSION_ID_BYTES)?;
        body.extend_from_slice(&frame.token.incarnation.to_be_bytes());
        body.extend_from_slice(&frame.sequence.to_be_bytes());
        put_string(&mut body, &frame.application, MAX_STREAM_COMPONENT_BYTES)?;
        put_string(&mut body, &frame.stream_name, MAX_STREAM_COMPONENT_BYTES)?;
        body.push(event_kind(frame.event.kind()));
        body.extend_from_slice(&frame.event.timestamp_ms().to_be_bytes());
        let payload_length = u32::try_from(frame.event.payload_len())
            .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        body.extend_from_slice(&payload_length.to_be_bytes());
        body.extend_from_slice(frame.event.payload());
        if body.len() > MAX_PACKET_BYTES {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        Ok(body)
    }

    fn decode_media(service_id: &str, body: &[u8]) -> Result<AutoPushFrame, RtmpAutoPushError> {
        if body.len() > MAX_PACKET_BYTES {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        let mut reader = Reader::new(body);
        let received_service = reader.string(MAX_SERVICE_ID_BYTES)?;
        if received_service != service_id {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        let worker_id = reader.array::<16>()?;
        let session_id = reader.string(MAX_SESSION_ID_BYTES)?;
        let incarnation = reader.u64()?;
        let sequence = reader.u64()?;
        let application = reader.string(MAX_STREAM_COMPONENT_BYTES)?;
        let stream_name = reader.string(MAX_STREAM_COMPONENT_BYTES)?;
        let kind = reader.u8()?;
        let timestamp = reader.u32()?;
        let payload_length =
            usize::try_from(reader.u32()?).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        let payload = reader.bytes(payload_length)?;
        reader.finish()?;
        let event = decode_event(kind, timestamp, payload)?;
        Ok(AutoPushFrame {
            application,
            stream_name,
            token: SourceToken {
                worker_id,
                session_id,
                incarnation,
            },
            sequence,
            event,
        })
    }

    pub(super) fn decode_event(
        kind: u8,
        timestamp: u32,
        payload: Vec<u8>,
    ) -> Result<MediaEvent, RtmpAutoPushError> {
        let event = match kind {
            0 => MediaEvent::from_wire(MediaEventKind::Metadata, timestamp, payload, None)
                .map_err(|_| RtmpAutoPushError::TransportUnavailable),
            1..=8 => {
                let event = if kind == 1 || kind == 2 {
                    MediaEvent::audio(timestamp, payload)
                } else {
                    MediaEvent::video(timestamp, payload)
                }
                .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
                if event_kind(event.kind()) != kind {
                    return Err(RtmpAutoPushError::TransportUnavailable);
                }
                Ok(event)
            }
            _ => Err(RtmpAutoPushError::TransportUnavailable),
        }?;
        Ok(event)
    }

    const fn event_kind(kind: MediaEventKind) -> u8 {
        match kind {
            MediaEventKind::Metadata => 0,
            MediaEventKind::AacSequenceHeader => 1,
            MediaEventKind::Audio => 2,
            MediaEventKind::AvcSequenceHeader => 3,
            MediaEventKind::HevcSequenceHeader => 4,
            MediaEventKind::Av1SequenceHeader => 5,
            MediaEventKind::VideoKeyframe => 6,
            MediaEventKind::VideoInterframe => 7,
            MediaEventKind::VideoDisposable => 8,
        }
    }

    fn write_packet(stream: &mut UnixStream, kind: u8, body: &[u8]) -> io::Result<()> {
        let length = u32::try_from(body.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "packet too large"))?;
        stream.write_all(&PROTOCOL_MAGIC)?;
        stream.write_all(&[PROTOCOL_VERSION, kind])?;
        stream.write_all(&length.to_be_bytes())?;
        stream.write_all(body)?;
        stream.flush()
    }

    enum PacketReadError {
        Timeout,
        Invalid,
        Io,
    }

    fn read_packet(stream: &mut UnixStream) -> Result<(u8, Vec<u8>), PacketReadError> {
        let mut header = [0; 10];
        stream
            .read_exact(&mut header)
            .map_err(|error| packet_read_error(&error))?;
        if header[..4] != PROTOCOL_MAGIC || header[4] != PROTOCOL_VERSION {
            return Err(PacketReadError::Invalid);
        }
        let length = usize::try_from(u32::from_be_bytes(
            header[6..10]
                .try_into()
                .expect("packet length has four bytes"),
        ))
        .map_err(|_| PacketReadError::Invalid)?;
        if length > MAX_PACKET_BYTES {
            return Err(PacketReadError::Invalid);
        }
        let mut body = vec![0; length];
        stream
            .read_exact(&mut body)
            .map_err(|error| packet_read_error(&error))?;
        Ok((header[5], body))
    }

    fn packet_read_error(error: &io::Error) -> PacketReadError {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            PacketReadError::Timeout
        } else {
            PacketReadError::Io
        }
    }

    fn auth_digest(
        secret: &[u8],
        label: &[u8],
        service_id: &[u8],
        first_worker: &WorkerId,
        nonce: &[u8; 16],
        second_worker: &[u8],
    ) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(secret);
        digest.update(label);
        digest.update(service_id);
        digest.update(first_worker);
        digest.update(nonce);
        digest.update(second_worker);
        digest.finalize().into()
    }

    fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
        if left.len() != right.len() {
            return false;
        }
        let mut difference = 0_u8;
        for (left, right) in left.iter().zip(right) {
            difference |= left ^ right;
        }
        difference == 0
    }

    fn secure_directory(path: &Path) -> Result<(), RtmpAutoPushError> {
        if path.as_os_str().is_empty() {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        if fs::symlink_metadata(path).is_err() {
            fs::create_dir_all(path).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
            fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))
                .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        }
        let metadata =
            fs::symlink_metadata(path).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        Ok(())
    }

    fn load_secret(path: &Path, create: bool) -> Result<Vec<u8>, RtmpAutoPushError> {
        let file = if create {
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)
            {
                Ok(mut file) => {
                    let secret = generated_secret();
                    file.write_all(&secret)
                        .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
                    file.sync_data()
                        .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
                    return Ok(secret.to_vec());
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => File::open(path),
                Err(_) => return Err(RtmpAutoPushError::CredentialsUnavailable),
            }
        } else {
            File::open(path)
        }
        .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
        let metadata = file
            .metadata()
            .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        let mut bytes = Vec::new();
        file.take((MAX_SECRET_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
        if bytes.len() != SECRET_BYTES {
            return Err(RtmpAutoPushError::CredentialsUnavailable);
        }
        Ok(bytes)
    }

    fn generated_secret() -> [u8; SECRET_BYTES] {
        let mut secret = [0; SECRET_BYTES];
        secret[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        secret[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        secret
    }

    fn endpoint_path(
        directory: &Path,
        service_hash: [u8; 4],
        worker_id: WorkerId,
    ) -> Result<PathBuf, RtmpAutoPushError> {
        let name = format!(
            "{SOCKET_PREFIX}{}-{}{SOCKET_SUFFIX}",
            hex(&service_hash),
            hex(&worker_id)
        );
        let path = directory.join(name);
        if path.as_os_str().to_str().is_none() || path.as_os_str().to_string_lossy().len() > 107 {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        Ok(path)
    }

    fn parse_endpoint(path: &Path, service_hash: [u8; 4]) -> Option<WorkerId> {
        let metadata = fs::symlink_metadata(path).ok()?;
        if !metadata.file_type().is_socket() || metadata.permissions().mode() & 0o077 != 0 {
            return None;
        }
        let name = path.file_name()?.to_str()?;
        let prefix = format!("{SOCKET_PREFIX}{}-", hex(&service_hash));
        let worker = name.strip_prefix(&prefix)?.strip_suffix(SOCKET_SUFFIX)?;
        decode_hex(worker)
    }

    fn hex(bytes: &[u8]) -> String {
        let mut result = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(result, "{byte:02x}");
        }
        result
    }

    fn decode_hex(value: &str) -> Option<WorkerId> {
        if value.len() != 32 {
            return None;
        }
        let mut bytes = [0; 16];
        for (index, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(bytes)
    }

    fn wait_for_wakeup(shared: &AutoPushShared, timeout: Duration) {
        let guard = shared
            .wake
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = shared
            .wake
            .1
            .wait_timeout(guard, timeout)
            .expect("RTMP auto-push wake mutex poisoned");
    }
}

#[cfg(unix)]
pub(crate) use unix::{AutoPushCoordinator, AutoPushPublisher};

#[cfg(not(unix))]
mod unsupported {
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
}

#[cfg(not(unix))]
pub(crate) use unsupported::{AutoPushCoordinator, AutoPushPublisher};

#[cfg(all(test, unix))]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, thread, time::Duration};

    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;
    use crate::{
        LiveHub, LiveHubLimits, MediaEvent, RtmpCapabilities, RtmpRegistry, SessionId, StreamKey,
        VideoCodecIdentifier,
    };
    use rml_rtmp::sessions::StreamMetadata;

    fn config(directory: &std::path::Path) -> RtmpAutoPushConfig {
        RtmpAutoPushConfig {
            enabled: true,
            socket_dir: directory.to_path_buf(),
            secret_file: None,
            reconnect_interval: Duration::from_millis(10),
            connect_timeout: Duration::from_millis(500),
            handshake_timeout: Duration::from_millis(500),
            max_peers: 4,
            max_queue_messages: 32,
            max_queue_bytes: 1024 * 1024,
            max_streams: 8,
        }
    }

    #[test]
    fn authenticated_local_workers_copy_media_without_forwarding_side_effects() {
        let directory = tempdir().expect("auto-push socket directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure auto-push socket directory");
        let source_hub = LiveHub::new(LiveHubLimits::default());
        let target_hub = LiveHub::new(LiveHubLimits::default());
        let source_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }));
        let target_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: false,
        }));
        let source = AutoPushCoordinator::new(
            config(directory.path()),
            "live",
            Arc::clone(&source_registry),
            source_hub.clone(),
            BTreeMap::new(),
        );
        let target = AutoPushCoordinator::new(
            config(directory.path()),
            "live",
            Arc::clone(&target_registry),
            target_hub.clone(),
            BTreeMap::new(),
        );
        target.ensure_started().expect("target worker socket");
        let key = StreamKey::new("live", "broadcast", "camera");
        let local_lease = source_hub
            .attach_publisher(key.clone())
            .expect("source publisher lease");
        let publisher = source
            .source(key.clone(), SessionId::new(), local_lease.incarnation())
            .expect("source admission")
            .expect("auto-push publisher");
        let subscription = target_hub
            .subscribe(key.clone())
            .expect("target subscriber");
        for _ in 0..100 {
            if source.status().peers > 0 && target.status().peers > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(source.status().peers, 1);
        assert_eq!(target.status().peers, 1);

        publish_snapshot_trace(&publisher);
        for _ in 0..100 {
            if target.status().remote_streams == 1
                && target_registry
                    .snapshot()
                    .streams
                    .first()
                    .is_some_and(|stream| {
                        stream.media.audio.payload_bytes_received == 3
                            && stream.media.video.payload_bytes_received == 6
                    })
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            target.status().remote_streams,
            1,
            "source status: {:?}, target status: {:?}",
            source.status(),
            target.status()
        );
        assert!(subscription.try_next().is_some());
        assert_eq!(target.status().source_streams, 0);
        assert_remote_snapshot(&target_registry);

        assert_reverse_copy(&target, &source, &target_hub, &source_hub);
    }

    fn publish_snapshot_trace(publisher: &AutoPushPublisher) {
        let mut metadata = StreamMetadata::new();
        metadata.audio_codec_id = Some(10);
        metadata.video_codec_id = Some(7);
        publisher.publish(&MediaEvent::metadata(metadata).expect("metadata event"), 1);
        publisher.publish(
            &MediaEvent::audio(1, Arc::<[u8]>::from(&[0xaf, 1, 0x10][..])).expect("audio event"),
            2,
        );
        publisher.publish(
            &MediaEvent::video(
                2,
                Arc::<[u8]>::from(&[0x91, b'h', b'v', b'c', b'1', 0x20][..]),
            )
            .expect("video event"),
            3,
        );
    }

    fn assert_remote_snapshot(registry: &RtmpRegistry) {
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.streams.len(), 1);
        let media = snapshot.streams[0].media;
        assert_eq!(media.audio.flv_codec_id, Some(10));
        assert_eq!(media.audio.last_rtmp_timestamp_ms, Some(1));
        assert!(media.audio.last_observed_at_unix_ms.is_some());
        assert_eq!(
            media.video.video_codec,
            Some(VideoCodecIdentifier::FourCc(*b"hvc1"))
        );
        assert_eq!(media.video.last_rtmp_timestamp_ms, Some(2));
        assert!(media.video.last_observed_at_unix_ms.is_some());
        assert_eq!(media.fanout_payload_bytes_queued, 0);
    }

    fn assert_reverse_copy(
        target: &AutoPushCoordinator,
        source: &AutoPushCoordinator,
        target_hub: &LiveHub,
        source_hub: &LiveHub,
    ) {
        let key = StreamKey::new("live", "broadcast", "reverse");
        let lease = target_hub
            .attach_publisher(key.clone())
            .expect("reverse source publisher lease");
        let publisher = target
            .source(key.clone(), SessionId::new(), lease.incarnation())
            .expect("reverse source admission")
            .expect("reverse auto-push publisher");
        let subscription = source_hub
            .subscribe(key)
            .expect("reverse target subscription");
        publisher.publish(
            &MediaEvent::audio(2, Arc::<[u8]>::from(&[0xaf, 1, 0x20][..])).expect("audio event"),
            1,
        );
        for _ in 0..100 {
            if source.status().remote_streams == 1 && subscription.queued_messages() > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(source.status().remote_streams, 1);
        assert!(subscription.try_next().is_some());
    }

    #[test]
    fn metadata_decode_is_best_effort_for_accepted_auto_push_frames() {
        let mut metadata = StreamMetadata::new();
        metadata.audio_codec_id = Some(10);
        metadata.video_codec_id = Some(7);
        let encoded = MediaEvent::metadata(metadata).expect("metadata event");
        let decoded = unix::decode_event(0, 0, encoded.payload().to_vec())
            .expect("structured metadata remains accepted");
        assert_eq!(
            decoded
                .stream_metadata()
                .and_then(|value| value.audio_codec_id),
            Some(10)
        );
        assert_eq!(
            decoded
                .stream_metadata()
                .and_then(|value| value.video_codec_id),
            Some(7)
        );

        let opaque =
            unix::decode_event(0, 0, vec![0xff]).expect("opaque metadata remains accepted");
        assert!(opaque.stream_metadata().is_none());
    }
}
