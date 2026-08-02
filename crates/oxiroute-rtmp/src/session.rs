use std::{collections::VecDeque, sync::Arc};

use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult},
};

use crate::{CatalogError, LiveHub, RtmpRegistry, SessionId};

#[path = "session_identity.rs"]
mod identity;
#[path = "session_playback.rs"]
mod playback;
#[path = "session_publish.rs"]
mod publish;
#[path = "session_runtime.rs"]
mod runtime;
#[path = "session_status.rs"]
mod status;

pub use playback::MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN;
pub use runtime::{
    RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpApplication, RtmpRecorderLifecycle, RtmpServiceRuntime,
    RtmpSessionPolicy,
};
pub use status::RtmpSessionError;

use runtime::SessionRole;
use status::{CONNECT_REJECTION_CODE, Rejection};

pub const MAX_INBOUND_CHUNK_SIZE: u32 = 1024 * 1024;
pub const MAX_INBOUND_MESSAGE_SIZE: usize = 8 * 1024 * 1024;

/// Incremental server-side RTMP connection with one live publisher or playback role.
pub struct RtmpSession {
    runtime: RtmpServiceRuntime,
    session_id: SessionId,
    handshake: Option<Handshake>,
    protocol: Option<ServerSession>,
    role: Option<SessionRole>,
}

impl RtmpSession {
    #[must_use]
    pub fn new(
        service_id: impl Into<String>,
        registry: Arc<RtmpRegistry>,
        hub: LiveHub,
        policy: RtmpSessionPolicy,
    ) -> Self {
        let service_id: String = service_id.into();
        let runtime = RtmpServiceRuntime::new(Arc::<str>::from(service_id), registry, hub, policy);
        Self::from_runtime(runtime)
    }

    fn from_runtime(runtime: RtmpServiceRuntime) -> Self {
        Self {
            runtime,
            session_id: SessionId::new(),
            handshake: Some(Handshake::new(PeerType::Server)),
            protocol: None,
            role: None,
        }
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns whether this connection currently owns a live playback role.
    ///
    /// Transport runtimes can use this to enable periodic playback polling only after a play
    /// request has established the role. [`Self::drain_playback`] remains strict and returns
    /// [`RtmpSessionError::NoActivePlayback`] when called without one.
    #[must_use]
    pub const fn is_playback_active(&self) -> bool {
        matches!(self.role, Some(SessionRole::Playback(_)))
    }

    /// Processes arbitrary contiguous bytes from this connection in wire order.
    ///
    /// # Errors
    ///
    /// Returns an error when the handshake, RTMP state machine, configured limits, media
    /// classification, fanout, or catalog transition fails. All returned packets must be written
    /// in order before reading more input.
    ///
    /// # Panics
    ///
    /// Panics only if the connection's internal handshake/session state invariant is broken.
    pub fn receive(
        &mut self,
        bytes: &[u8],
        at_unix_ms: u64,
    ) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
        self.observe_role_at(at_unix_ms);
        if self.protocol.is_some() {
            return self.receive_session_bytes(bytes, at_unix_ms);
        }

        let handshake = self
            .handshake
            .as_mut()
            .expect("a session without a protocol must still be handshaking");
        match handshake.process_bytes(bytes)? {
            HandshakeProcessResult::InProgress { response_bytes } => Ok(non_empty(response_bytes)),
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                self.handshake = None;
                let mut config = ServerSessionConfig::new();
                config.chunk_size = self.runtime.outbound_chunk_size();
                config.max_inbound_chunk_size = MAX_INBOUND_CHUNK_SIZE as usize;
                config.max_inbound_message_size = MAX_INBOUND_MESSAGE_SIZE;
                let (protocol, startup) = ServerSession::new(config)?;
                self.protocol = Some(protocol);

                let mut outbound = non_empty(response_bytes);
                outbound.extend(self.process_results(startup, at_unix_ms)?);
                if !remaining_bytes.is_empty() {
                    outbound.extend(self.receive_session_bytes(&remaining_bytes, at_unix_ms)?);
                }
                Ok(outbound)
            }
        }
    }

    /// Serializes at most `maximum_events` queued playback events in queue order.
    ///
    /// The subscription lock is released by `try_next` before `rml_rtmp` serializes each event.
    ///
    /// # Errors
    ///
    /// Returns an error when queued media cannot be serialized for the active RTMP stream.
    pub fn drain_playback(
        &mut self,
        maximum_events: usize,
    ) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
        playback::drain(self, maximum_events)
    }

    /// Detaches this connection's active publisher or viewer, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog no longer contains the role owned by this session.
    pub fn close(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        self.detach_role(at_unix_ms)
    }

    fn receive_session_bytes(
        &mut self,
        bytes: &[u8],
        at_unix_ms: u64,
    ) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
        let results = self
            .protocol
            .as_mut()
            .expect("completed handshake must create a protocol session")
            .handle_input(bytes)?;
        self.process_results(results, at_unix_ms)
    }

    fn process_results(
        &mut self,
        results: Vec<ServerSessionResult>,
        at_unix_ms: u64,
    ) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
        let mut pending: VecDeque<_> = results.into();
        let mut outbound = Vec::new();

        while let Some(result) = pending.pop_front() {
            match result {
                ServerSessionResult::OutboundResponse(packet) => outbound.push(packet.bytes),
                ServerSessionResult::RaisedEvent(event) => {
                    let generated = self.handle_event(event, at_unix_ms)?;
                    for result in generated.into_iter().rev() {
                        pending.push_front(result);
                    }
                }
                ServerSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }

        Ok(outbound)
    }

    fn handle_event(
        &mut self,
        event: ServerSessionEvent,
        at_unix_ms: u64,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        match event {
            ServerSessionEvent::ConnectionRequested {
                request_id,
                app_name,
            } => self.handle_connection_request(request_id, &app_name),
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                app_name,
                stream_key,
                mode,
            } => {
                publish::handle_request(self, request_id, &app_name, &stream_key, &mode, at_unix_ms)
            }
            ServerSessionEvent::PublishStreamFinished {
                app_name,
                stream_key,
            } => {
                if self.publisher_matches(&app_name, &stream_key) {
                    self.detach_role(at_unix_ms)?;
                }
                Ok(Vec::new())
            }
            ServerSessionEvent::PlayStreamRequested {
                request_id,
                app_name,
                stream_key,
                stream_id,
                ..
            } => playback::handle_request(
                self,
                request_id,
                &app_name,
                &stream_key,
                stream_id,
                at_unix_ms,
            ),
            ServerSessionEvent::PlayStreamFinished {
                app_name,
                stream_key,
            } => {
                if self.playback_matches(&app_name, &stream_key) {
                    self.detach_role(at_unix_ms)?;
                }
                Ok(Vec::new())
            }
            ServerSessionEvent::ClientChunkSizeChanged { new_chunk_size }
                if new_chunk_size > MAX_INBOUND_CHUNK_SIZE =>
            {
                Err(RtmpSessionError::InboundChunkTooLarge(new_chunk_size))
            }
            ServerSessionEvent::StreamMetadataChanged {
                app_name,
                stream_key,
                metadata,
            } if self.publisher_matches(&app_name, &stream_key) => {
                self.publisher_mut().handle_metadata(metadata, at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::AudioDataReceived {
                app_name,
                stream_key,
                data,
                timestamp,
            } if self.publisher_matches(&app_name, &stream_key) => {
                self.publisher_mut()
                    .handle_audio(&data, timestamp, at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::VideoDataReceived {
                app_name,
                stream_key,
                data,
                timestamp,
            } if self.publisher_matches(&app_name, &stream_key) => {
                self.publisher_mut()
                    .handle_video(&data, timestamp, at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::ClientChunkSizeChanged { .. }
            | ServerSessionEvent::ReleaseStreamRequested { .. }
            | ServerSessionEvent::StreamMetadataChanged { .. }
            | ServerSessionEvent::AudioDataReceived { .. }
            | ServerSessionEvent::VideoDataReceived { .. }
            | ServerSessionEvent::UnhandleableAmf0Command { .. }
            | ServerSessionEvent::AcknowledgementReceived { .. }
            | ServerSessionEvent::PingResponseReceived { .. } => Ok(Vec::new()),
        }
    }

    fn handle_connection_request(
        &mut self,
        request_id: u32,
        application: &str,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        if let Err(error) = identity::validate_application(application) {
            return self.reject_request(request_id, status::connection_path(error));
        }
        if self.runtime.application(application).is_some() {
            Ok(self.protocol_mut().accept_request(request_id)?)
        } else {
            self.reject_request(
                request_id,
                Rejection::new(CONNECT_REJECTION_CODE, "RTMP application is not configured"),
            )
        }
    }

    fn reject_request(
        &mut self,
        request_id: u32,
        rejection: Rejection,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        Ok(self
            .protocol_mut()
            .reject_request(request_id, rejection.code, rejection.description)?)
    }

    fn detach_role(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        self.role
            .take()
            .map_or(Ok(()), |mut role| role.release(at_unix_ms))
    }

    fn observe_role_at(&mut self, at_unix_ms: u64) {
        if let Some(role) = &mut self.role {
            role.observe_at(at_unix_ms);
        }
    }

    fn publisher_matches(&self, application: &str, protocol_name: &str) -> bool {
        matches!(
            &self.role,
            Some(SessionRole::Publisher(publisher))
                if publisher.matches(application, protocol_name)
        )
    }

    fn playback_matches(&self, application: &str, protocol_name: &str) -> bool {
        matches!(
            &self.role,
            Some(SessionRole::Playback(playback))
                if playback.matches(application, protocol_name)
        )
    }

    fn publisher_mut(&mut self) -> &mut publish::PublishSession {
        let Some(SessionRole::Publisher(publisher)) = &mut self.role else {
            unreachable!("matched publisher event requires an active publisher role");
        };
        publisher
    }

    fn protocol_mut(&mut self) -> &mut ServerSession {
        self.protocol
            .as_mut()
            .expect("server events require a protocol session")
    }
}

impl Drop for RtmpSession {
    fn drop(&mut self) {
        // Recorder controllers must submit their workers before the session releases its reaper.
        self.role.take();
    }
}

fn non_empty(bytes: Vec<u8>) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        Vec::new()
    } else {
        vec![bytes]
    }
}
