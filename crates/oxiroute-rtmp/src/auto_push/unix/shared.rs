#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct AutoPushShared {
    pub(super) config: RtmpAutoPushConfig,
    pub(super) service_id: Arc<str>,
    pub(super) service_hash: [u8; 4],
    pub(super) worker_id: WorkerId,
    pub(super) registry: Arc<RtmpRegistry>,
    pub(super) default_hub: LiveHub,
    pub(super) application_hubs: BTreeMap<String, LiveHub>,
    pub(super) stop: AtomicBool,
    pub(super) start_lock: Mutex<()>,
    pub(super) next_connection_id: AtomicU64,
    pub(super) wake: (Mutex<bool>, Condvar),
    pub(super) secret: Mutex<Option<Arc<[u8]>>>,
    pub(super) handles: Mutex<Vec<thread::JoinHandle<()>>>,
    pub(super) state: Mutex<AutoPushState>,
}

#[derive(Default)]
pub(super) struct AutoPushState {
    pub(super) started: bool,
    pub(super) closed: bool,
    endpoint: Option<PathBuf>,
    pub(super) sources: BTreeMap<StreamKey, SourceState>,
    pub(super) remotes: BTreeMap<StreamKey, RemoteStream>,
    peers: BTreeMap<WorkerId, PeerConnection>,
    pub(super) counters: Counters,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Counters {
    frames_sent: u64,
    frames_received: u64,
    frames_dropped: u64,
    authentication_failures: u64,
    reconnects: u64,
    pub(super) last_failure: Option<RtmpAutoPushError>,
}

#[derive(Clone)]
pub(super) struct SourceState {
    pub(super) token: SourceToken,
    pub(super) cache: AutoPushCache,
}

pub(super) struct RemoteStream {
    pub(super) token: SourceToken,
    peer: WorkerId,
    pub(super) session_id: SessionId,
    pub(super) stream_id: crate::StreamId,
    pub(super) lease: crate::PublisherLease,
    pub(super) registration: PublisherRegistration,
    pub(super) sequence: u64,
    pub(super) media: MediaSnapshotAccumulator,
    pub(super) hub: LiveHub,
    pub(super) registry: Arc<RtmpRegistry>,
}

#[derive(Clone)]
struct PeerConnection {
    id: u64,
    queue: Option<Arc<PeerQueue>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceToken {
    pub(super) worker_id: WorkerId,
    pub(super) session_id: String,
    pub(super) incarnation: u64,
}

impl AutoPushShared {
    pub(super) fn lock_state(&self) -> std::sync::MutexGuard<'_, AutoPushState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn start(self: &Arc<Self>) -> Result<(), RtmpAutoPushError> {
        secure_directory(&self.config.socket_dir)?;
        let default_secret_path = self.config.socket_dir.join(".secret");
        let secret_path = self
            .config
            .secret_file
            .as_deref()
            .unwrap_or(&default_secret_path);
        let secret = load_secret(secret_path, self.config.secret_file.is_none())?;
        let endpoint = endpoint_path(&self.config.socket_dir, self.service_hash, self.worker_id)?;
        let listener =
            UnixListener::bind(&endpoint).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
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

    pub(super) fn publish(&self, key: &StreamKey, token: &SourceToken, frame: &AutoPushFrame) {
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

    pub(super) fn remove_source(&self, key: &StreamKey, token: &SourceToken) {
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

    pub(super) fn close(&self) {
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

    pub(super) fn status(&self) -> RtmpAutoPushStatus {
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
        if shared.stop.load(Ordering::Acquire) || shared.lock_state().peers.contains_key(&worker_id)
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
