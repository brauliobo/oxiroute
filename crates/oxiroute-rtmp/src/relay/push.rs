#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) struct RtmpRelayController {
    pub(super) shared: Arc<RelayShared>,
}

pub(super) struct RelayShared {
    pub(super) destination: RtmpDestination,
    pub(super) config: RtmpRelayConfig,
    pub(super) resolver: Option<Arc<RtmpDestinationResolver>>,
    pub(super) state: Mutex<RelayState>,
    pub(super) available: Condvar,
}

pub(super) struct RelayState {
    pub(super) accepting: bool,
    pub(super) waiting_for_keyframe: bool,
    pub(super) queue: VecDeque<MediaEvent>,
    pub(super) queue_bytes: usize,
    pub(super) queue_started_at: Option<Instant>,
    pub(super) cache: RelayCache,
    pub(super) phase: RtmpRelayPhase,
    pub(super) last_failure: Option<RtmpRelayFailure>,
    pub(super) connection_attempts: u64,
    pub(super) connections: u64,
    pub(super) reconnects: u64,
    pub(super) events_enqueued: u64,
    pub(super) events_sent: u64,
    pub(super) events_dropped: u64,
    pub(super) payload_bytes_sent: u64,
    pub(super) dns_refresh_attempts: u64,
    pub(super) dns_refresh_successes: u64,
    pub(super) dns_refresh_failures: u64,
    pub(super) last_dns_refresh_failure: Option<RtmpDnsRefreshFailure>,
}

#[derive(Default)]
pub(super) struct RelayCache {
    metadata: Option<MediaEvent>,
    aac_header: Option<MediaEvent>,
    video_headers: BTreeMap<VideoCodec, MediaEvent>,
    keyframe: Option<MediaEvent>,
    latest_audio: Option<MediaEvent>,
}

impl RtmpRelayController {
    pub(crate) fn start(destination: RtmpDestination, config: RtmpRelayConfig) -> Arc<Self> {
        let resolver = config.dns_resolver.clone();
        let shared = Arc::new(RelayShared {
            destination,
            config,
            resolver,
            state: Mutex::new(RelayState {
                accepting: true,
                waiting_for_keyframe: false,
                queue: VecDeque::new(),
                queue_bytes: 0,
                queue_started_at: None,
                cache: RelayCache::default(),
                phase: RtmpRelayPhase::Connecting,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_enqueued: 0,
                events_sent: 0,
                events_dropped: 0,
                payload_bytes_sent: 0,
                dns_refresh_attempts: 0,
                dns_refresh_successes: 0,
                dns_refresh_failures: 0,
                last_dns_refresh_failure: None,
            }),
            available: Condvar::new(),
        });
        let controller = Arc::new(Self { shared });
        if relay_executor()
            .admit(Arc::clone(&controller.shared))
            .is_err()
        {
            let mut state = controller.shared.lock();
            state.accepting = false;
            state.phase = RtmpRelayPhase::Stopped;
            state.last_failure = Some(RtmpRelayFailure::Thread);
        }
        controller
    }

    pub(crate) fn try_enqueue(&self, event: MediaEvent) {
        let mut state = self.shared.lock();
        if !state.accepting {
            return;
        }
        let codec_header_changed = state.cache.update(&event);
        if codec_header_changed {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
            state.waiting_for_keyframe = true;
        }
        if state.waiting_for_keyframe {
            if event.kind() != MediaEventKind::VideoKeyframe {
                state.events_dropped = state.events_dropped.saturating_add(1);
                return;
            }
            let bootstrap = state.cache.bootstrap();
            if fits_queue(&bootstrap, &self.shared.config) {
                for cached in bootstrap {
                    state.queue_bytes += cached.payload_len();
                    state.queue.push_back(cached);
                }
                state.queue_started_at = (!state.queue.is_empty()).then(Instant::now);
                state.waiting_for_keyframe = false;
                drop(state);
                self.shared.available.notify_one();
                return;
            }
            state.events_dropped = state.events_dropped.saturating_add(1);
            return;
        }
        let exceeds_messages = state.queue.len() >= self.shared.config.max_queue_messages;
        let queue_bytes = state.queue_bytes.checked_add(event.payload_len());
        if exceeds_messages
            || queue_bytes.is_none_or(|bytes| bytes > self.shared.config.max_queue_bytes)
        {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state
                .events_dropped
                .saturating_add(discarded)
                .saturating_add(1);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
            state.waiting_for_keyframe = !state.cache.video_headers.is_empty();
            if event.kind() == MediaEventKind::VideoKeyframe {
                let bootstrap = state.cache.bootstrap();
                if fits_queue(&bootstrap, &self.shared.config) {
                    for cached in bootstrap {
                        state.queue_bytes += cached.payload_len();
                        state.queue.push_back(cached);
                    }
                    state.queue_started_at = (!state.queue.is_empty()).then(Instant::now);
                    state.waiting_for_keyframe = false;
                }
            }
            self.shared.available.notify_one();
            return;
        }

        state.queue_bytes = queue_bytes.expect("bounded relay queue byte sum was checked");
        if state.queue.is_empty() {
            state.queue_started_at = Some(Instant::now());
        }
        state.queue.push_back(event);
        state.events_enqueued = state.events_enqueued.saturating_add(1);
        drop(state);
        self.shared.available.notify_one();
    }

    pub(crate) fn status(&self) -> RtmpRelayStatus {
        let state = self.shared.lock();
        let mut destination = self.shared.destination.clone();
        if let Some(resolver) = &self.shared.resolver {
            destination.address = resolver.address();
        }
        RtmpRelayStatus {
            destination,
            phase: state.phase,
            last_failure: state.last_failure,
            queue_messages: state.queue.len(),
            queue_bytes: state.queue_bytes,
            connection_attempts: state.connection_attempts,
            connections: state.connections,
            reconnects: state.reconnects,
            events_enqueued: state.events_enqueued,
            events_sent: state.events_sent,
            events_dropped: state.events_dropped,
            payload_bytes_sent: state.payload_bytes_sent,
            dns_refresh_attempts: state.dns_refresh_attempts,
            dns_refresh_successes: state.dns_refresh_successes,
            dns_refresh_failures: state.dns_refresh_failures,
            last_dns_refresh_failure: state.last_dns_refresh_failure,
        }
    }

    pub(crate) fn deactivate(&self) {
        {
            let mut state = self.shared.lock();
            state.accepting = false;
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
        }
        self.shared.available.notify_all();
    }
}

impl Drop for RtmpRelayController {
    fn drop(&mut self) {
        self.deactivate();
    }
}

impl RelayShared {
    pub(super) fn lock(&self) -> MutexGuard<'_, RelayState> {
        self.state.lock().expect("RTMP relay state mutex poisoned")
    }

    fn is_accepting(&self) -> bool {
        self.lock().accepting
    }

    fn set_phase(&self, phase: RtmpRelayPhase) {
        self.lock().phase = phase;
    }

    fn record_failure(&self, failure: RtmpRelayFailure) {
        let mut state = self.lock();
        state.last_failure = Some(failure);
        state.phase = RtmpRelayPhase::Backoff;
    }

    fn refresh_address(&self) -> SocketAddr {
        let Some(resolver) = &self.resolver else {
            return self.destination.address;
        };
        match resolver.refresh_if_due() {
            RtmpDnsRefresh::NotDue => {}
            RtmpDnsRefresh::Refreshed(_) => {
                let mut state = self.lock();
                state.dns_refresh_attempts = state.dns_refresh_attempts.saturating_add(1);
                state.dns_refresh_successes = state.dns_refresh_successes.saturating_add(1);
                state.last_dns_refresh_failure = None;
            }
            RtmpDnsRefresh::Failed(failure) => {
                let mut state = self.lock();
                state.dns_refresh_attempts = state.dns_refresh_attempts.saturating_add(1);
                state.dns_refresh_failures = state.dns_refresh_failures.saturating_add(1);
                state.last_dns_refresh_failure = Some(failure);
            }
        }
        resolver.address()
    }

    fn wait_backoff(&self) -> bool {
        let state = self.lock();
        let (state, _) = self
            .available
            .wait_timeout_while(state, self.config.reconnect_interval, |state| {
                state.accepting
            })
            .expect("RTMP relay state mutex poisoned during backoff");
        state.accepting
    }

    pub(super) fn next_event(&self) -> Option<MediaEvent> {
        let state = self.lock();
        let mut state = self
            .available
            .wait_timeout_while(state, RELAY_QUEUE_POLL, |state| {
                state.accepting && state.queue.is_empty()
            })
            .expect("RTMP relay state mutex poisoned while waiting")
            .0;
        if state
            .queue_started_at
            .is_some_and(|started| started.elapsed() >= self.config.buffer_duration)
        {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.queue_started_at = None;
            state.waiting_for_keyframe = !state.cache.video_headers.is_empty();
            return None;
        }
        let event = state.queue.pop_front()?;
        state.queue_bytes -= event.payload_len();
        if state.queue.is_empty() {
            state.queue_started_at = None;
        }
        Some(event)
    }

    fn take_bootstrap(&self) -> Vec<MediaEvent> {
        let mut state = self.lock();
        let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
        state.events_dropped = state.events_dropped.saturating_add(discarded);
        state.queue.clear();
        state.queue_bytes = 0;
        state.queue_started_at = None;
        state.cache.bootstrap()
    }

    fn record_sent(&self, event: &MediaEvent) {
        let mut state = self.lock();
        state.events_sent = state.events_sent.saturating_add(1);
        state.payload_bytes_sent = state
            .payload_bytes_sent
            .saturating_add(event.payload_len() as u64);
    }
}

impl RelayCache {
    fn update(&mut self, event: &MediaEvent) -> bool {
        let mut codec_header_changed = false;
        match event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(event.clone()),
            MediaEventKind::AacSequenceHeader => self.aac_header = Some(event.clone()),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {
                if let Some(codec) = event.video_codec() {
                    self.keyframe = None;
                    self.video_headers.insert(codec, event.clone());
                    codec_header_changed = true;
                }
            }
            MediaEventKind::VideoKeyframe => self.keyframe = Some(event.clone()),
            MediaEventKind::Audio => self.latest_audio = Some(event.clone()),
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {}
        }
        codec_header_changed
    }

    fn bootstrap(&self) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(4);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_header.iter().cloned());
        if let Some(keyframe) = &self.keyframe {
            events.extend(
                keyframe
                    .video_codec()
                    .and_then(|codec| self.video_headers.get(&codec))
                    .cloned(),
            );
            events.push(keyframe.clone());
        } else {
            events.extend(self.latest_audio.iter().cloned());
        }
        events
    }
}

fn relay_executor() -> &'static BoundedExecutor<Arc<RelayShared>> {
    static EXECUTOR: OnceLock<BoundedExecutor<Arc<RelayShared>>> = OnceLock::new();
    EXECUTOR.get_or_init(|| {
        BoundedExecutor::new(
            MAX_QUEUED_RTMP_RELAYS,
            RTMP_RELAY_WORKER_THREADS,
            "rtmp-relay-worker",
            "RTMP relay executor mutex poisoned",
            "shared RTMP relay worker must start",
            |shared| run_relay(shared),
        )
    })
}

fn fits_queue(events: &[MediaEvent], config: &RtmpRelayConfig) -> bool {
    events.len() <= config.max_queue_messages
        && events
            .iter()
            .try_fold(0_usize, |total, event| {
                total.checked_add(event.payload_len())
            })
            .is_some_and(|bytes| bytes <= config.max_queue_bytes)
}

fn run_relay(shared: &RelayShared) {
    while shared.is_accepting() {
        {
            let mut state = shared.lock();
            state.phase = RtmpRelayPhase::Connecting;
            state.connection_attempts = state.connection_attempts.saturating_add(1);
        }
        let address = shared.refresh_address();
        let Ok(mut stream) = client::connect_stream(
            &shared.destination.host,
            address,
            shared.destination.transport,
            shared.config.connect_timeout,
            shared.config.handshake_timeout,
        ) else {
            shared.record_failure(RtmpRelayFailure::Connect);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        };
        if establish_transport(&mut stream, shared.config.handshake_timeout).is_err() {
            shared.record_failure(RtmpRelayFailure::Handshake);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        }
        let mut session = match establish_publish_session(
            &mut stream,
            &shared.destination,
            shared.config.handshake_timeout,
        ) {
            Ok(session) => session,
            Err(failure) => {
                shared.record_failure(failure);
                if !shared.wait_backoff() {
                    break;
                }
                continue;
            }
        };
        {
            let mut state = shared.lock();
            state.connections = state.connections.saturating_add(1);
            state.reconnects = state.connections.saturating_sub(1);
            state.phase = RtmpRelayPhase::Publishing;
            state.last_failure = None;
        }

        let mut failed = None;
        for event in shared.take_bootstrap() {
            if let Err(failure) = publish_event(&mut stream, &mut session, &event) {
                failed = Some(failure);
                break;
            }
            shared.record_sent(&event);
        }
        while failed.is_none() && shared.is_accepting() {
            if let Err(failure) = process_peer_input(&mut stream, &mut session) {
                failed = Some(failure);
                break;
            }
            let Some(event) = shared.next_event() else {
                continue;
            };
            if let Err(failure) = publish_event(&mut stream, &mut session, &event) {
                failed = Some(failure);
                break;
            }
            shared.record_sent(&event);
        }
        if let Some(failure) = failed {
            shared.record_failure(failure);
            if !shared.wait_backoff() {
                break;
            }
        }
    }
    shared.set_phase(RtmpRelayPhase::Stopped);
}

pub(super) fn establish_transport(stream: &mut RtmpStream, timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;
    let mut handshake = Handshake::new(PeerType::Client);
    let hello = handshake
        .generate_outbound_p0_and_p1()
        .map_err(io::Error::other)?;
    stream.write_all(&hello)?;
    let mut response = [0; HANDSHAKE_RESPONSE_BYTES];
    stream.read_exact(&mut response)?;
    let finish = match handshake
        .process_bytes(&response)
        .map_err(io::Error::other)?
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RTMP server handshake remained incomplete",
            ));
        }
    };
    stream.write_all(&finish)?;
    stream.flush()
}

fn establish_publish_session(
    stream: &mut RtmpStream,
    destination: &RtmpDestination,
    timeout: Duration,
) -> Result<ClientSession, RtmpRelayFailure> {
    let mut config = ClientSessionConfig::new();
    config.chunk_size = 4_096;
    config.playback_buffer_length_ms = destination.options.playback_buffer_ms;
    config
        .flash_version
        .clone_from(&destination.options.flash_version);
    config.tc_url = Some(destination.options.tc_url.clone().unwrap_or_else(|| {
        format!(
            "{}://{}/{}",
            match destination.transport {
                RtmpTransport::Rtmp => "rtmp",
                RtmpTransport::Rtmps => "rtmps",
            },
            destination.host,
            destination.application
        )
    }));
    apply_credentials(&mut config, &destination.options)?;
    let (mut session, initial) =
        ClientSession::new(config).map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, initial)?;
    let connect = session
        .request_connection(destination.application.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![connect], timeout, |event| {
        matches!(event, ClientSessionEvent::ConnectionRequestAccepted)
    })?;
    let publish = session
        .request_publishing(destination.stream_name.clone(), PublishRequestType::Live)
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![publish], timeout, |event| {
        matches!(event, ClientSessionEvent::PublishRequestAccepted)
    })?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .map_err(|_| RtmpRelayFailure::Transport)?;
    Ok(session)
}

pub(super) fn apply_credentials(
    config: &mut ClientSessionConfig,
    options: &RtmpClientOptions,
) -> Result<(), RtmpRelayFailure> {
    let Some(credential) = &options.credential else {
        return Ok(());
    };
    config.username = Some(credential.username().to_owned());
    config.password = Some(
        String::from_utf8(credential.secret().to_vec()).map_err(|_| RtmpRelayFailure::Session)?,
    );
    Ok(())
}

pub(super) fn await_event(
    stream: &mut RtmpStream,
    session: &mut ClientSession,
    initial: Vec<ClientSessionResult>,
    timeout: Duration,
    predicate: impl Fn(&ClientSessionEvent) -> bool,
) -> Result<(), RtmpRelayFailure> {
    write_results(stream, initial)?;
    let deadline = Instant::now() + timeout;
    let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
    while Instant::now() < deadline {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| RtmpRelayFailure::Transport)?;
        if count == 0 {
            return Err(RtmpRelayFailure::Transport);
        }
        let results = session
            .handle_input(&buffer[..count])
            .map_err(|_| RtmpRelayFailure::Session)?;
        let mut accepted = false;
        let mut outbound = Vec::new();
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => outbound.push(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => accepted |= predicate(&event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        write_packets(stream, outbound)?;
        if accepted {
            return Ok(());
        }
    }
    Err(RtmpRelayFailure::Session)
}

fn process_peer_input(
    stream: &mut RtmpStream,
    session: &mut ClientSession,
) -> Result<(), RtmpRelayFailure> {
    let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
    match stream.read(&mut buffer) {
        Ok(0) => Err(RtmpRelayFailure::Transport),
        Ok(count) => {
            let results = session
                .handle_input(&buffer[..count])
                .map_err(|_| RtmpRelayFailure::Session)?;
            write_results(stream, results)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(RtmpRelayFailure::Transport),
    }
}

fn publish_event(
    stream: &mut RtmpStream,
    session: &mut ClientSession,
    event: &MediaEvent,
) -> Result<(), RtmpRelayFailure> {
    let result = match event.kind() {
        MediaEventKind::Metadata => session.publish_metadata(
            event
                .stream_metadata()
                .expect("metadata events retain decoded metadata"),
        ),
        MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => session.publish_audio_data(
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            false,
        ),
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => session.publish_video_data(
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            event.kind() == MediaEventKind::VideoDisposable,
        ),
    }
    .map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, vec![result])
}

pub(super) fn write_results(
    stream: &mut RtmpStream,
    results: Vec<ClientSessionResult>,
) -> Result<(), RtmpRelayFailure> {
    write_packets(
        stream,
        results.into_iter().filter_map(|result| match result {
            ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
            ClientSessionResult::RaisedEvent(_)
            | ClientSessionResult::UnhandleableMessageReceived(_) => None,
        }),
    )
}

fn write_packets(
    stream: &mut RtmpStream,
    packets: impl IntoIterator<Item = Vec<u8>>,
) -> Result<(), RtmpRelayFailure> {
    for packet in packets {
        stream
            .write_all(&packet)
            .map_err(|_| RtmpRelayFailure::Transport)?;
    }
    stream.flush().map_err(|_| RtmpRelayFailure::Transport)
}
