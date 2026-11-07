#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) struct RtmpPullController {
    shared: Arc<PullShared>,
}

struct PullShared {
    service_id: Arc<str>,
    target: RtmpPullTarget,
    resolver: Option<Arc<RtmpDestinationResolver>>,
    registry: Arc<RtmpRegistry>,
    hub: LiveHub,
    state: Mutex<PullState>,
    available: Condvar,
}

struct PullState {
    accepting: bool,
    phase: RtmpRelayPhase,
    last_failure: Option<RtmpRelayFailure>,
    connection_attempts: u64,
    connections: u64,
    reconnects: u64,
    events_received: u64,
    payload_bytes_received: u64,
    dns_refresh_attempts: u64,
    dns_refresh_successes: u64,
    dns_refresh_failures: u64,
    last_dns_refresh_failure: Option<RtmpDnsRefreshFailure>,
}

impl RtmpPullController {
    pub(crate) fn start(
        service_id: Arc<str>,
        target: RtmpPullTarget,
        registry: Arc<RtmpRegistry>,
        hub: LiveHub,
    ) -> Arc<Self> {
        let resolver = target.config.dns_resolver.clone();
        let shared = Arc::new(PullShared {
            service_id,
            target,
            resolver,
            registry,
            hub,
            state: Mutex::new(PullState {
                accepting: true,
                phase: RtmpRelayPhase::Connecting,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_received: 0,
                payload_bytes_received: 0,
                dns_refresh_attempts: 0,
                dns_refresh_successes: 0,
                dns_refresh_failures: 0,
                last_dns_refresh_failure: None,
            }),
            available: Condvar::new(),
        });
        let controller = Arc::new(Self { shared });
        if pull_executor()
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

    pub(crate) fn deactivate(&self) {
        let mut state = self.shared.lock();
        state.accepting = false;
        drop(state);
        self.shared.available.notify_all();
    }
}

impl Drop for RtmpPullController {
    fn drop(&mut self) {
        self.deactivate();
    }
}

impl PullShared {
    fn lock(&self) -> MutexGuard<'_, PullState> {
        self.state.lock().expect("RTMP pull state mutex poisoned")
    }

    fn is_accepting(&self) -> bool {
        self.lock().accepting
    }

    fn wait_backoff(&self) -> bool {
        let state = self.lock();
        let (state, _) = self
            .available
            .wait_timeout_while(state, self.target.config.reconnect_interval, |state| {
                state.accepting
            })
            .expect("RTMP pull state mutex poisoned during backoff");
        state.accepting
    }

    fn record_failure(&self, failure: RtmpRelayFailure) {
        let mut state = self.lock();
        state.last_failure = Some(failure);
        state.phase = RtmpRelayPhase::Backoff;
    }

    fn set_phase(&self, phase: RtmpRelayPhase) {
        self.lock().phase = phase;
    }

    fn refresh_address(&self) -> SocketAddr {
        let Some(resolver) = &self.resolver else {
            return self.target.address;
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
}

fn pull_executor() -> &'static BoundedExecutor<Arc<PullShared>> {
    static EXECUTOR: OnceLock<BoundedExecutor<Arc<PullShared>>> = OnceLock::new();
    EXECUTOR.get_or_init(|| {
        BoundedExecutor::new(
            MAX_QUEUED_RTMP_RELAYS,
            RTMP_RELAY_WORKER_THREADS,
            "rtmp-pull-worker",
            "RTMP pull executor mutex poisoned",
            "shared RTMP pull worker must start",
            |shared| run_pull(shared),
        )
    })
}

#[allow(
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::too_many_lines,
    reason = "the pull worker keeps connection, RTMP session, and publisher-incarnation cleanup in one bounded loop"
)]
fn run_pull(shared: &PullShared) {
    while shared.is_accepting() {
        {
            let mut state = shared.lock();
            state.phase = RtmpRelayPhase::Connecting;
            state.connection_attempts = state.connection_attempts.saturating_add(1);
        }
        let address = shared.refresh_address();
        let Ok(mut stream) = client::connect_stream(
            &shared.target.host,
            address,
            shared.target.transport,
            shared.target.config.connect_timeout,
            shared.target.config.handshake_timeout,
        ) else {
            shared.record_failure(RtmpRelayFailure::Connect);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        };
        if establish_transport(&mut stream, shared.target.config.handshake_timeout).is_err() {
            shared.record_failure(RtmpRelayFailure::Handshake);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        }
        let mut session = match establish_pull_session(
            &mut stream,
            &shared.target,
            shared.target.config.handshake_timeout,
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
        let now = unix_time_ms();
        let key = StreamKey::new(
            shared.service_id.as_ref(),
            &shared.target.local_application,
            &shared.target.local_stream_name,
        );
        let publisher_session_id = SessionId::new();
        let (lease, mut registration) = {
            let _transaction = shared.hub.lock_roles();
            let lease = match shared.hub.attach_publisher(key.clone()) {
                Ok(lease) => lease,
                Err(_) => {
                    shared.record_failure(RtmpRelayFailure::Source);
                    if !shared.wait_backoff() {
                        break;
                    }
                    continue;
                }
            };
            let registration =
                match shared
                    .registry
                    .register_publisher(key, publisher_session_id, Vec::new(), now)
                {
                    Ok(registration) => registration,
                    Err(_) => {
                        drop(lease);
                        shared.record_failure(RtmpRelayFailure::Source);
                        if !shared.wait_backoff() {
                            break;
                        }
                        continue;
                    }
                };
            (lease, registration)
        };
        {
            let mut state = shared.lock();
            state.connections = state.connections.saturating_add(1);
            state.reconnects = state.connections.saturating_sub(1);
            state.phase = RtmpRelayPhase::Pulling;
            state.last_failure = None;
        }

        let stream_id = registration.stream_id();
        let mut media = MediaSnapshotAccumulator::default();
        let mut sequence = 0_u64;
        let mut failed = None;
        let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
        while failed.is_none() && shared.is_accepting() {
            let count = match stream.read(&mut buffer) {
                Ok(0) => {
                    failed = Some(RtmpRelayFailure::Transport);
                    continue;
                }
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => {
                    failed = Some(RtmpRelayFailure::Transport);
                    continue;
                }
            };
            let results = match session.handle_input(&buffer[..count]) {
                Ok(results) => results,
                Err(_) => {
                    failed = Some(RtmpRelayFailure::Session);
                    continue;
                }
            };
            for result in results {
                match result {
                    ClientSessionResult::OutboundResponse(packet) => {
                        if stream.write_all(&packet.bytes).is_err() {
                            failed = Some(RtmpRelayFailure::Transport);
                            break;
                        }
                    }
                    ClientSessionResult::RaisedEvent(event) => {
                        let Some(event) = pull_media_event(event) else {
                            continue;
                        };
                        sequence = sequence.saturating_add(1);
                        let at_unix_ms = unix_time_ms();
                        media.observe(&event, at_unix_ms);
                        if lease.publish(event.clone()).is_err()
                            || shared
                                .registry
                                .update_media_sample(
                                    stream_id,
                                    publisher_session_id,
                                    sequence,
                                    media.snapshot(0),
                                    at_unix_ms,
                                )
                                .is_err()
                        {
                            failed = Some(RtmpRelayFailure::Source);
                            break;
                        }
                        let mut state = shared.lock();
                        state.events_received = state.events_received.saturating_add(1);
                        state.payload_bytes_received = state
                            .payload_bytes_received
                            .saturating_add(event.payload_len() as u64);
                    }
                    ClientSessionResult::UnhandleableMessageReceived(_) => {}
                }
            }
            if stream.flush().is_err() {
                failed = Some(RtmpRelayFailure::Transport);
            }
        }
        let at_unix_ms = unix_time_ms();
        let _ = registration.release(at_unix_ms);
        drop(lease);
        if shared.is_accepting()
            && let Some(failure) = failed
        {
            shared.record_failure(failure);
            if !shared.wait_backoff() {
                break;
            }
        }
    }
    shared.set_phase(RtmpRelayPhase::Stopped);
}

fn establish_pull_session(
    stream: &mut RtmpStream,
    target: &RtmpPullTarget,
    timeout: Duration,
) -> Result<ClientSession, RtmpRelayFailure> {
    let mut config = ClientSessionConfig::new();
    config
        .flash_version
        .clone_from(&target.options.flash_version);
    config.playback_buffer_length_ms = target.options.playback_buffer_ms;
    config.tc_url = Some(target.options.tc_url.clone().unwrap_or_else(|| {
        format!(
            "{}://{}/{}",
            match target.transport {
                RtmpTransport::Rtmp => "rtmp",
                RtmpTransport::Rtmps => "rtmps",
            },
            target.host,
            target.source_application
        )
    }));
    apply_credentials(&mut config, &target.options)?;
    let (mut session, initial) =
        ClientSession::new(config).map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, initial)?;
    let connect = session
        .request_connection(target.source_application.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![connect], timeout, |event| {
        matches!(event, ClientSessionEvent::ConnectionRequestAccepted)
    })?;
    let play = session
        .request_playback(target.source_stream_name.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![play], timeout, |event| {
        matches!(event, ClientSessionEvent::PlaybackRequestAccepted)
    })?;
    Ok(session)
}

fn pull_media_event(event: ClientSessionEvent) -> Option<MediaEvent> {
    match event {
        ClientSessionEvent::StreamMetadataReceived { metadata } => {
            MediaEvent::metadata(metadata).ok()
        }
        ClientSessionEvent::AudioDataReceived { timestamp, data } => {
            MediaEvent::audio(timestamp.value, Arc::<[u8]>::from(data.as_ref())).ok()
        }
        ClientSessionEvent::VideoDataReceived { timestamp, data } => {
            MediaEvent::video(timestamp.value, Arc::<[u8]>::from(data.as_ref())).ok()
        }
        ClientSessionEvent::ConnectionRequestAccepted
        | ClientSessionEvent::ConnectionRequestRejected { .. }
        | ClientSessionEvent::PlaybackRequestAccepted
        | ClientSessionEvent::PublishRequestAccepted
        | ClientSessionEvent::UnhandleableAmf0Command { .. }
        | ClientSessionEvent::UnknownTransactionResultReceived { .. }
        | ClientSessionEvent::UnhandleableOnStatusCode { .. }
        | ClientSessionEvent::AcknowledgementReceived { .. }
        | ClientSessionEvent::PingResponseReceived { .. } => None,
    }
}
