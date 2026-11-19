#[allow(clippy::wildcard_imports)]
use super::*;
use oxiroute_rtmp::{
    CatalogEvent, EndedStreamSnapshot, PublisherEndReason, RecorderTransition,
    RecorderTransitionKind, StreamId, StreamKey,
};
use oxiroute_server::RtmpSessionTracking;

const RTMP_READ_BUFFER_SIZE: usize = 16 * 1024;
const RTMP_PLAYBACK_DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const RTMP_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RTMP_PUBLISHER_LIVENESS_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const RTMP_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct RtmpIngest {
    listener: String,
    admission: ListenerRuntime,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    control: RtmpControlHandle,
    runtime: RtmpServiceHandle,
    service: Arc<RtmpServicePlan>,
}

impl RtmpIngest {
    pub(super) fn new(
        listener: String,
        runtime: RtmpServiceHandle,
        control: RtmpControlHandle,
        service: Arc<RtmpServicePlan>,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            listener,
            admission: ListenerRuntime::new(metrics.clone()),
            generation,
            metrics,
            control,
            runtime,
            service,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RtmpAccessEvent {
    Connect,
    Disconnect,
    Publish,
    Play,
    Record,
    Relay,
}

impl RtmpAccessEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::Publish => "publish",
            Self::Play => "play",
            Self::Record => "record",
            Self::Relay => "relay",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RtmpAccessResult {
    Accepted,
    Rejected,
    Closed,
    Failed,
}

impl RtmpAccessResult {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RtmpAccessCounters {
    bytes_received: u64,
    pub(super) bytes_sent: u64,
    messages_received: u64,
    pub(super) messages_sent: u64,
}

#[async_trait]
impl ServerApp for RtmpIngest {
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.admission.accepting() && self.generation.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .admission
            .admit(&self.generation, RuntimeReferenceKind::Rtmp)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected RTMP connection: {error}");
                return None;
            }
        };
        Some(Box::new(lease))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .admission
            .admit_owned(&self.generation, RuntimeReferenceKind::Rtmp)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected RTMP connection: {error}");
                return None;
            }
        };
        Some(Box::new(lease))
    }

    #[allow(clippy::too_many_lines)]
    // `async_trait` boxes an `async fn`, which is what `ServerApp::process_new` avoids.
    #[allow(clippy::manual_async_fn)]
    fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> impl std::future::Future<Output = Option<Stream>> + Send {
        async move {
            let Some(mut at_unix_ms) = unix_time_ms() else {
                warn!("cannot start RTMP session because the system clock is invalid");
                return None;
            };
            let session_started_at = Instant::now();
            let peer = downstream.get_socket_digest().and_then(|digest| {
                digest
                    .peer_addr()
                    .and_then(|address| address.as_inet().copied())
            });
            let tracking = RtmpSessionTracking::open(&self.metrics, &downstream, peer);
            let mut session = self
                .runtime
                .session_with_peer_addr(peer.map(|address| address.ip()));
            let mut previous_snapshot = session.client_snapshot();
            let mut access_counters = RtmpAccessCounters::default();
            let mut last_failure_code = None;
            let mut buffer = [0; RTMP_READ_BUFFER_SIZE];
            let mut shutdown = shutdown.clone();
            let mut playback_drain = interval(RTMP_PLAYBACK_DRAIN_INTERVAL);
            playback_drain.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut control_poll = interval(RTMP_CONTROL_POLL_INTERVAL);
            control_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut publisher_liveness = interval(RTMP_PUBLISHER_LIVENESS_CHECK_INTERVAL);
            publisher_liveness.set_missed_tick_behavior(MissedTickBehavior::Skip);

            let end_reason = loop {
                let outbound = tokio::select! {
                    _ = shutdown.changed() => {
                        break PublisherEndReason::Shutdown;
                    }
                    _ = control_poll.tick() => {
                        if session.is_publisher_active() {
                            emit_catalog_events(&self.control);
                        }
                        if session.take_control_action().is_some() {
                            break PublisherEndReason::Dropped;
                        }
                        Vec::new()
                    }
                    _ = publisher_liveness.tick() => {
                        let Some(now_unix_ms) = unix_time_ms() else {
                            warn!("closing RTMP session because the system clock is invalid");
                            break PublisherEndReason::Failed;
                        };
                        at_unix_ms = now_unix_ms;
                        if session.is_publisher_stale(now_unix_ms) {
                            warn!("closing stale RTMP publisher session after media inactivity");
                            break PublisherEndReason::Stale;
                        }
                        Vec::new()
                    }
                    _ = playback_drain.tick(), if session.is_playback_active() => {
                        match session.drain_playback(MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN) {
                            Ok(outbound) => outbound,
                            Err(error) => {
                                warn!("RTMP playback serialization failed: {error}");
                                break PublisherEndReason::Failed;
                            }
                        }
                    }
                    read = downstream.read(&mut buffer) => {
                        let bytes_read = match read {
                            Ok(0) => break PublisherEndReason::Closed,
                            Ok(bytes_read) => bytes_read,
                            Err(error) => {
                                warn!("RTMP transport read failed: {error}");
                                break PublisherEndReason::Failed;
                            }
                        };
                        let Ok(bytes_received) = u64::try_from(bytes_read) else {
                            warn!("RTMP read size exceeds the supported metrics range");
                            break PublisherEndReason::Failed;
                        };
                        access_counters.bytes_received = access_counters
                            .bytes_received
                            .saturating_add(bytes_received);
                        access_counters.messages_received = access_counters
                            .messages_received
                            .saturating_add(1);
                        tracking.received(bytes_received);
                        if let Err(error) = self.metrics.record_bytes_received(bytes_received) {
                            warn!("could not account for RTMP ingress: {error}");
                            break PublisherEndReason::Failed;
                        }
                        let Some(now_unix_ms) = unix_time_ms() else {
                            warn!("closing RTMP session because the system clock is invalid");
                            break PublisherEndReason::Failed;
                        };
                        at_unix_ms = now_unix_ms;

                        match session.receive(&buffer[..bytes_read], at_unix_ms) {
                            Ok(outbound) => {
                                if let Some(code) = session.take_access_failure_code()
                                    && let Some(snapshot) = session.client_snapshot()
                                {
                                    log_rtmp_access_event(
                                        &self.service,
                                        &self.listener,
                                        rejection_event(code),
                                        RtmpAccessResult::Rejected,
                                        &snapshot,
                                        &access_counters,
                                        session_started_at,
                                        at_unix_ms,
                                        Some(rejection_failure_code(code)),
                                    );
                                }
                                outbound
                            }
                            Err(error) => {
                                let failure_code = rtmp_session_failure_code(&error);
                                last_failure_code = Some(failure_code);
                                if let Some(snapshot) = session.client_snapshot()
                                    && snapshot.connected
                                {
                                    log_rtmp_access_event(
                                        &self.service,
                                        &self.listener,
                                        event_for_role(snapshot.role),
                                        RtmpAccessResult::Failed,
                                        &snapshot,
                                        &access_counters,
                                        session_started_at,
                                        at_unix_ms,
                                        Some(failure_code),
                                    );
                                }
                                warn!("RTMP session failed: {error}");
                                break PublisherEndReason::Failed;
                            }
                        }
                    }
                };
                if let Some(current_snapshot) = session.client_snapshot() {
                    log_rtmp_state_transition(
                        &self.service,
                        &self.listener,
                        &self.control,
                        previous_snapshot.as_ref(),
                        &current_snapshot,
                        &access_counters,
                        session_started_at,
                        at_unix_ms,
                    );
                    previous_snapshot = Some(current_snapshot);
                }
                if outbound.is_empty() {
                    continue;
                }
                if let Err(error) = write_rtmp_packets(
                    &mut downstream,
                    &outbound,
                    &self.metrics,
                    &tracking,
                    &mut access_counters,
                )
                .await
                {
                    last_failure_code = Some("transport_write_failed");
                    if let Some(snapshot) = session.client_snapshot()
                        && snapshot.connected
                    {
                        log_rtmp_access_event(
                            &self.service,
                            &self.listener,
                            event_for_role(snapshot.role),
                            RtmpAccessResult::Failed,
                            &snapshot,
                            &access_counters,
                            session_started_at,
                            at_unix_ms,
                            last_failure_code,
                        );
                    }
                    warn!("RTMP transport write failed: {error}");
                    break PublisherEndReason::Failed;
                }
            };
            session.set_end_reason(end_reason);

            let terminal_snapshot = session.client_snapshot();
            if let Some(snapshot) = terminal_snapshot.as_ref()
                && snapshot.connected
                && snapshot.role == RtmpSessionRole::Publisher
            {
                log_rtmp_auxiliary_failures(
                    &self.service,
                    &self.listener,
                    &self.control,
                    snapshot,
                    &access_counters,
                    session_started_at,
                    at_unix_ms,
                );
            }
            let close_result = session.close(at_unix_ms);
            emit_catalog_events(&self.control);
            let close_failed = if let Err(error) = close_result {
                last_failure_code = Some("session_close_failed");
                warn!("could not detach RTMP media role: {error}");
                true
            } else {
                false
            };
            if let Some(snapshot) = terminal_snapshot {
                if snapshot.connected {
                    log_rtmp_access_event(
                        &self.service,
                        &self.listener,
                        RtmpAccessEvent::Disconnect,
                        if close_failed {
                            RtmpAccessResult::Failed
                        } else {
                            RtmpAccessResult::Closed
                        },
                        &snapshot,
                        &access_counters,
                        session_started_at,
                        at_unix_ms,
                        last_failure_code,
                    );
                } else {
                    log_rtmp_access_event(
                        &self.service,
                        &self.listener,
                        RtmpAccessEvent::Connect,
                        RtmpAccessResult::Rejected,
                        &snapshot,
                        &access_counters,
                        session_started_at,
                        at_unix_ms,
                        Some(last_failure_code.unwrap_or("connect_rejected")),
                    );
                }
            }
            tracking.finish(close_failed || ended_in_failure(end_reason));
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn log_rtmp_state_transition(
    service: &RtmpServicePlan,
    listener: &str,
    control: &RtmpControlHandle,
    previous: Option<&RtmpClientSnapshot>,
    current: &RtmpClientSnapshot,
    counters: &RtmpAccessCounters,
    session_started_at: Instant,
    at_unix_ms: u64,
) {
    if !previous.is_some_and(|snapshot| snapshot.connected) && current.connected {
        log_rtmp_access_event(
            service,
            listener,
            RtmpAccessEvent::Connect,
            RtmpAccessResult::Accepted,
            current,
            counters,
            session_started_at,
            at_unix_ms,
            None,
        );
    }
    let previous_role = previous.map_or(RtmpSessionRole::Client, |snapshot| snapshot.role);
    if previous_role == current.role {
        return;
    }
    match current.role {
        RtmpSessionRole::Publisher => {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Publish,
                RtmpAccessResult::Accepted,
                current,
                counters,
                session_started_at,
                at_unix_ms,
                None,
            );
            let catalog = control.catalog_snapshot();
            if let Some(stream) = catalog.streams.iter().find(|stream| {
                stream
                    .publisher
                    .is_some_and(|publisher| publisher.session_id == current.session_id)
            }) {
                if stream.recorders.iter().any(|recorder| {
                    matches!(
                        recorder.phase,
                        RecorderPhase::Starting { .. } | RecorderPhase::Recording { .. }
                    )
                }) {
                    log_rtmp_access_event(
                        service,
                        listener,
                        RtmpAccessEvent::Record,
                        RtmpAccessResult::Accepted,
                        current,
                        counters,
                        session_started_at,
                        at_unix_ms,
                        None,
                    );
                }
                if !stream.relays.is_empty() {
                    log_rtmp_access_event(
                        service,
                        listener,
                        RtmpAccessEvent::Relay,
                        RtmpAccessResult::Accepted,
                        current,
                        counters,
                        session_started_at,
                        at_unix_ms,
                        None,
                    );
                }
            }
        }
        RtmpSessionRole::Subscriber => {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Play,
                RtmpAccessResult::Accepted,
                current,
                counters,
                session_started_at,
                at_unix_ms,
                None,
            );
        }
        RtmpSessionRole::Client => {}
    }
}

fn log_rtmp_auxiliary_failures(
    service: &RtmpServicePlan,
    listener: &str,
    control: &RtmpControlHandle,
    snapshot: &RtmpClientSnapshot,
    counters: &RtmpAccessCounters,
    session_started_at: Instant,
    at_unix_ms: u64,
) {
    let catalog = control.catalog_snapshot();
    let Some(stream) = catalog.streams.iter().find(|stream| {
        stream
            .publisher
            .is_some_and(|publisher| publisher.session_id == snapshot.session_id)
    }) else {
        return;
    };
    for recorder in &stream.recorders {
        if let RecorderPhase::Failed { code, .. } = recorder.phase {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Record,
                RtmpAccessResult::Failed,
                snapshot,
                counters,
                session_started_at,
                at_unix_ms,
                Some(recorder_failure_code(code)),
            );
        }
    }
    for relay in &stream.relays {
        if let Some(failure) = relay.status.last_failure {
            log_rtmp_access_event(
                service,
                listener,
                RtmpAccessEvent::Relay,
                RtmpAccessResult::Failed,
                snapshot,
                counters,
                session_started_at,
                at_unix_ms,
                Some(relay_failure_code(failure)),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn log_rtmp_access_event(
    service: &RtmpServicePlan,
    listener: &str,
    event: RtmpAccessEvent,
    result: RtmpAccessResult,
    snapshot: &RtmpClientSnapshot,
    counters: &RtmpAccessCounters,
    session_started_at: Instant,
    at_unix_ms: u64,
    failure_code: Option<&str>,
) {
    emit_rtmp_access(
        event.as_str(),
        result.as_str(),
        RtmpEventDetail {
            service: Some(service.service_id().to_owned()),
            application: snapshot.application.clone(),
            stream: snapshot.stream_name.clone(),
            stream_id: snapshot_stream_id(service, snapshot),
            session_id: Some(snapshot.session_id.to_string()),
            role: Some(snapshot.role.as_str().to_owned()),
            reason: failure_code.map(str::to_owned),
            recorder: None,
        },
    );
    let duration_ms = u64::try_from(session_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let value = serde_json::json!({
        "timestampUnixMs": at_unix_ms,
        "event": event.as_str(),
        "result": result.as_str(),
        "listener": listener,
        "service": service.service_id(),
        "application": snapshot.application.as_deref(),
        "stream": snapshot.stream_name.as_deref(),
        "sessionId": snapshot.session_id.to_string(),
        "role": snapshot.role.as_str(),
        "bytesReceived": counters.bytes_received,
        "bytesSent": counters.bytes_sent,
        "messagesReceived": counters.messages_received,
        "messagesSent": counters.messages_sent,
        "durationMs": duration_ms,
        "failureCode": failure_code,
    });
    if let Err(error) = service.write_rtmp_access_event(&value) {
        warn!("RTMP access log write failed: {error}");
    }
}

fn snapshot_stream_id(service: &RtmpServicePlan, snapshot: &RtmpClientSnapshot) -> Option<String> {
    let key = StreamKey::new(
        service.service_id(),
        snapshot.application.as_deref()?,
        snapshot.stream_name.as_deref()?,
    );
    Some(StreamId::for_key(&key).to_string())
}

fn emit_catalog_events(control: &RtmpControlHandle) {
    for event in control.take_catalog_events() {
        match event {
            CatalogEvent::PublisherEnded(ended) => emit_publisher_ended(&ended),
            CatalogEvent::Recorder(transition) => emit_recorder_transition(&transition),
        }
    }
}

fn emit_publisher_ended(ended: &EndedStreamSnapshot) {
    let (reason, outcome) = match ended.end_reason {
        PublisherEndReason::Closed => ("closed", "closed"),
        PublisherEndReason::Dropped => ("dropped", "closed"),
        PublisherEndReason::Shutdown => ("shutdown", "closed"),
        PublisherEndReason::Stale => ("stale", "failed"),
        PublisherEndReason::Failed => ("failed", "failed"),
    };
    emit_rtmp_access(
        "publish_end",
        outcome,
        RtmpEventDetail {
            service: Some(ended.key.server_id.clone()),
            application: Some(ended.key.application.clone()),
            stream: Some(ended.key.name.clone()),
            stream_id: Some(ended.stream_id.to_string()),
            session_id: Some(ended.publisher_session_id.to_string()),
            role: Some("publisher".to_owned()),
            reason: Some(reason.to_owned()),
            recorder: None,
        },
    );
}

fn emit_recorder_transition(transition: &RecorderTransition) {
    let (event, outcome, reason) = match transition.kind {
        RecorderTransitionKind::Started => ("recorder_started", "accepted", None),
        RecorderTransitionKind::Recovered => ("recorder_started", "accepted", Some("recovered")),
        RecorderTransitionKind::Stopped => ("recorder_stopped", "closed", None),
        RecorderTransitionKind::Failed(code) => (
            "recorder_failed",
            "failed",
            Some(recorder_failure_code(code).trim_start_matches("record_")),
        ),
    };
    emit_rtmp_access(
        event,
        outcome,
        RtmpEventDetail {
            service: Some(transition.key.server_id.clone()),
            application: Some(transition.key.application.clone()),
            stream: Some(transition.key.name.clone()),
            stream_id: Some(transition.stream_id.to_string()),
            session_id: transition.publisher_session_id.map(|id| id.to_string()),
            role: Some("publisher".to_owned()),
            reason: reason.map(str::to_owned),
            recorder: transition.recorder_name.clone(),
        },
    );
}

const fn ended_in_failure(reason: PublisherEndReason) -> bool {
    matches!(
        reason,
        PublisherEndReason::Stale | PublisherEndReason::Failed
    )
}

fn event_for_role(role: RtmpSessionRole) -> RtmpAccessEvent {
    match role {
        RtmpSessionRole::Client => RtmpAccessEvent::Connect,
        RtmpSessionRole::Publisher => RtmpAccessEvent::Publish,
        RtmpSessionRole::Subscriber => RtmpAccessEvent::Play,
    }
}

fn rejection_event(code: &str) -> RtmpAccessEvent {
    match code {
        "NetStream.Publish.BadName" => RtmpAccessEvent::Publish,
        "NetStream.Play.Failed" | "NetStream.Play.StreamNotFound" => RtmpAccessEvent::Play,
        _ => RtmpAccessEvent::Connect,
    }
}

fn rejection_failure_code(code: &str) -> &'static str {
    match code {
        "NetConnection.Connect.Rejected" => "connect_rejected",
        "NetStream.Publish.BadName" => "publish_rejected",
        "NetStream.Play.StreamNotFound" => "play_stream_not_found",
        "NetStream.Play.Failed" => "play_rejected",
        _ => "request_rejected",
    }
}

fn rtmp_session_failure_code(error: &RtmpSessionError) -> &'static str {
    match error {
        RtmpSessionError::Handshake(_) => "handshake_failed",
        RtmpSessionError::Session(_) => "protocol_failed",
        RtmpSessionError::Catalog(_) => "catalog_failed",
        RtmpSessionError::LiveHub(_) => "fanout_failed",
        RtmpSessionError::MediaEvent(_) => "media_failed",
        RtmpSessionError::InboundChunkTooLarge(_) => "inbound_chunk_too_large",
        RtmpSessionError::MediaSequenceExhausted => "media_sequence_exhausted",
        RtmpSessionError::MissingMetadata => "missing_metadata",
        RtmpSessionError::NoActivePlayback => "playback_unavailable",
        RtmpSessionError::Callback(_) => "callback_failed",
        RtmpSessionError::Vod(_) => "vod_failed",
        RtmpSessionError::MessageStream { .. } => "message_stream_failed",
    }
}

fn recorder_failure_code(code: RecorderErrorCode) -> &'static str {
    match code {
        RecorderErrorCode::OpenFailed => "record_open_failed",
        RecorderErrorCode::QuotaFiles => "record_quota_files",
        RecorderErrorCode::QuotaBytes => "record_quota_bytes",
        RecorderErrorCode::WriteFailed => "record_write_failed",
        RecorderErrorCode::CloseFailed => "record_close_failed",
        RecorderErrorCode::BackendUnavailable => "record_backend_unavailable",
        RecorderErrorCode::FileSyncFailed => "record_file_sync_failed",
        RecorderErrorCode::PublishFailed => "record_publish_failed",
        RecorderErrorCode::DirectorySyncFailed => "record_directory_sync_failed",
        RecorderErrorCode::QueueDiscontinuity => "record_queue_discontinuity",
        RecorderErrorCode::UnsupportedCodec => "record_unsupported_codec",
        RecorderErrorCode::ShutdownTimedOut => "record_shutdown_timeout",
        RecorderErrorCode::WorkerPanicked => "record_worker_panicked",
        RecorderErrorCode::StalePublisher => "record_stale_publisher",
    }
}

fn relay_failure_code(failure: RtmpRelayFailure) -> &'static str {
    match failure {
        RtmpRelayFailure::Policy => "relay_policy",
        RtmpRelayFailure::Connect => "relay_connect_failed",
        RtmpRelayFailure::Handshake => "relay_handshake_failed",
        RtmpRelayFailure::Session => "relay_session_failed",
        RtmpRelayFailure::Transport => "relay_transport_failed",
        RtmpRelayFailure::Source => "relay_source_failed",
        RtmpRelayFailure::Thread => "relay_worker_unavailable",
    }
}

pub(super) async fn write_rtmp_packets(
    downstream: &mut Stream,
    packets: &[Vec<u8>],
    metrics: &ListenerMetrics,
    tracking: &RtmpSessionTracking,
    counters: &mut RtmpAccessCounters,
) -> io::Result<()> {
    timeout(RTMP_WRITE_TIMEOUT, async {
        for packet in packets {
            let mut remaining = packet.as_slice();
            while !remaining.is_empty() {
                let bytes_written = downstream.write(remaining).await?;
                if bytes_written == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "RTMP transport accepted no response bytes",
                    ));
                }
                let bytes_sent = u64::try_from(bytes_written)
                    .map_err(|_| io::Error::other("RTMP write size exceeds u64"))?;
                metrics
                    .record_bytes_sent(bytes_sent)
                    .map_err(io::Error::other)?;
                tracking.sent(bytes_sent);
                counters.bytes_sent = counters.bytes_sent.saturating_add(bytes_sent);
                remaining = &remaining[bytes_written..];
            }
            counters.messages_sent = counters.messages_sent.saturating_add(1);
        }
        downstream.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RTMP write deadline exceeded"))?
}
