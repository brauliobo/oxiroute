use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    sync::Arc,
};

use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult},
};

use crate::{
    CatalogError, LiveHub, RtmpCallbackContext, RtmpCallbackError, RtmpCallbackEvent,
    RtmpCallbackPolicy, RtmpClientSnapshot, RtmpMessageStreamSnapshot, RtmpRegistry,
    RtmpSessionControlAction, RtmpSessionRole, SessionId,
};

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
#[path = "vod_playback.rs"]
mod vod_playback;

pub use playback::MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN;
pub use runtime::{
    RTMP_STALE_PUBLISHER_THRESHOLD_MS, RtmpAccessAction, RtmpAccessPolicy, RtmpAccessRule,
    RtmpApplication, RtmpNetwork, RtmpRecorderLifecycle, RtmpServiceRuntime, RtmpSessionCeilings,
    RtmpSessionLimits, RtmpSessionPolicy, RtmpTokenPolicy,
};
pub use status::RtmpSessionError;

use crate::session_control::RtmpSessionControl;
use runtime::SessionRole;
use status::{CONNECT_REJECTION_CODE, Rejection};

pub const MAX_INBOUND_CHUNK_SIZE: u32 = 1024 * 1024;
pub const MAX_INBOUND_MESSAGE_SIZE: usize = 8 * 1024 * 1024;
pub const MAX_INBOUND_AMF0_DEPTH: usize = 32;
pub const MAX_INBOUND_AMF0_CONTAINER_ENTRIES: usize = 1_024;
pub const MAX_INBOUND_AMF0_VALUES: usize = 4_096;
pub const MAX_INBOUND_AMF0_STRING_BYTES: usize = u16::MAX as usize;
pub const MAX_RTMP_MESSAGE_STREAMS: usize = 8;

/// Incremental server-side RTMP connection with bounded message-stream roles.
pub struct RtmpSession {
    runtime: RtmpServiceRuntime,
    session_id: SessionId,
    handshake: Option<Handshake>,
    protocol: Option<ServerSession>,
    roles: BTreeMap<u32, SessionRole>,
    peer_addr: Option<IpAddr>,
    connection_lease: Option<runtime::ApplicationSessionLease>,
    connected_application: Option<Arc<str>>,
    control: Option<Arc<RtmpSessionControl>>,
    last_rejection_code: Option<&'static str>,
    last_callback_update_at_unix_ms: BTreeMap<u32, u64>,
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
        Self::from_runtime(runtime, None)
    }

    pub(super) fn from_runtime(runtime: RtmpServiceRuntime, peer_addr: Option<IpAddr>) -> Self {
        let session_id = SessionId::new();
        let control =
            runtime
                .registry()
                .register_session(session_id, runtime.service_id(), peer_addr);
        Self {
            runtime,
            session_id,
            handshake: Some(Handshake::new(PeerType::Server)),
            protocol: None,
            roles: BTreeMap::new(),
            peer_addr,
            connection_lease: None,
            connected_application: None,
            control,
            last_rejection_code: None,
            last_callback_update_at_unix_ms: BTreeMap::new(),
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
    pub fn is_playback_active(&self) -> bool {
        self.roles
            .values()
            .any(|role| matches!(role, SessionRole::Playback(_) | SessionRole::VodPlayback(_)))
    }

    /// Returns whether the active publisher has emitted no metadata or media for the stale
    /// publisher threshold.
    #[must_use]
    pub fn is_publisher_stale(&self, at_unix_ms: u64) -> bool {
        self.roles.values().any(|role| {
            matches!(role, SessionRole::Publisher(publisher) if publisher.is_stale(at_unix_ms))
        })
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
        self.observe_role_at(at_unix_ms)?;
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
                let limits = self.runtime.inbound_limits();
                config.chunk_size = self.runtime.outbound_chunk_size();
                config.window_ack_size = limits.window_ack_size;
                config.max_inbound_chunk_size = limits.max_inbound_chunk_size as usize;
                config.max_inbound_message_size = limits.max_inbound_message_size;
                config.max_active_streams = MAX_RTMP_MESSAGE_STREAMS;
                let (protocol, startup) =
                    ServerSession::new_with_amf0_limits(config, limits.amf0_limits())?;
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

    /// Detaches every active message-stream role owned by this connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog no longer contains the role owned by this session.
    pub fn close(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        let result = self.detach_roles(at_unix_ms);
        if let Some(application) = self.connected_application.take() {
            let context = self.callback_context(&application, None, None);
            let _ = self
                .runtime
                .callbacks()
                .notify(RtmpCallbackEvent::Disconnect, &context);
            if let Some(callbacks) = self
                .runtime
                .application(&application)
                .map(|application| application.callbacks().clone())
            {
                let _ = callbacks.notify(RtmpCallbackEvent::Disconnect, &context);
            }
        }
        result
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

    #[allow(clippy::too_many_lines)]
    fn handle_event(
        &mut self,
        event: ServerSessionEvent,
        at_unix_ms: u64,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        match event {
            ServerSessionEvent::ConnectionRequested {
                request_id,
                app_name,
            } => self.handle_connection_request(request_id, &app_name, at_unix_ms),
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                app_name,
                stream_key,
                mode,
                stream_id,
            } => {
                let result = publish::handle_request(
                    self,
                    request_id,
                    &app_name,
                    &stream_key,
                    &mode,
                    stream_id,
                    at_unix_ms,
                );
                if result.is_ok() {
                    self.sync_control_state();
                }
                result
            }
            ServerSessionEvent::PublishStreamFinished {
                app_name,
                stream_key,
                stream_id,
            }
            | ServerSessionEvent::PlayStreamFinished {
                app_name,
                stream_key,
                stream_id,
            } => self.detach_role_if_matches(stream_id, &app_name, &stream_key, at_unix_ms),
            ServerSessionEvent::PlayStreamRequested {
                request_id,
                app_name,
                stream_key,
                stream_id,
                ..
            } => {
                let result = playback::handle_request(
                    self,
                    request_id,
                    &app_name,
                    &stream_key,
                    stream_id,
                    at_unix_ms,
                );
                if result.is_ok() {
                    self.sync_control_state();
                }
                result
            }
            ServerSessionEvent::ClientChunkSizeChanged { new_chunk_size }
                if new_chunk_size > self.runtime.inbound_limits().max_inbound_chunk_size =>
            {
                Err(RtmpSessionError::InboundChunkTooLarge(new_chunk_size))
            }
            ServerSessionEvent::StreamMetadataChanged {
                app_name,
                stream_key,
                metadata,
                stream_id,
            } if self.publisher_matches(stream_id, &app_name, &stream_key) => {
                self.publisher_mut(stream_id)
                    .handle_metadata(metadata, at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::AudioDataReceived {
                app_name,
                stream_key,
                data,
                timestamp,
                stream_id,
            } if self.publisher_matches(stream_id, &app_name, &stream_key) => {
                self.publisher_mut(stream_id)
                    .handle_audio(&data, timestamp, at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::VideoDataReceived {
                app_name,
                stream_key,
                data,
                timestamp,
                stream_id,
            } if self.publisher_matches(stream_id, &app_name, &stream_key) => {
                self.publisher_mut(stream_id)
                    .handle_video(&data, timestamp, at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::ClientChunkSizeChanged { .. }
            | ServerSessionEvent::ReleaseStreamRequested { .. }
            | ServerSessionEvent::UnhandleableAmf0Command { .. }
            | ServerSessionEvent::AcknowledgementReceived { .. }
            | ServerSessionEvent::PingResponseReceived { .. } => Ok(Vec::new()),
            ServerSessionEvent::StreamMetadataChanged { stream_id, .. }
            | ServerSessionEvent::AudioDataReceived { stream_id, .. }
            | ServerSessionEvent::VideoDataReceived { stream_id, .. } => {
                Err(RtmpSessionError::MessageStream { stream_id })
            }
        }
    }

    fn handle_connection_request(
        &mut self,
        request_id: u32,
        application: &str,
        at_unix_ms: u64,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        if let Err(error) = identity::validate_application(application) {
            return self.reject_request(request_id, status::connection_path(error));
        }
        if self.connected_application.is_some() {
            return self.reject_request(
                request_id,
                Rejection::new(
                    CONNECT_REJECTION_CODE,
                    "RTMP connection is already attached to an application",
                ),
            );
        }
        if self.runtime.application(application).is_none() {
            self.reject_request(
                request_id,
                Rejection::new(CONNECT_REJECTION_CODE, "RTMP application is not configured"),
            )
        } else {
            let context = self.callback_context(application, None, None);
            if let Err(error) =
                self.authorize_callbacks(application, RtmpCallbackEvent::Connect, &context)
            {
                return self.reject_request(request_id, status::connection_callback(error));
            }
            let connection_lease = match self.runtime.acquire_connection(application) {
                Ok(lease) => lease,
                Err(error) => {
                    return self.reject_request(request_id, status::connection_limit(error));
                }
            };
            let accepted = match self.protocol_mut().accept_request(request_id) {
                Ok(accepted) => accepted,
                Err(error) => return Err(error.into()),
            };
            self.connection_lease = Some(connection_lease);
            self.connected_application = Some(Arc::from(application));
            if let Some(control) = &self.control {
                control.connected(application, at_unix_ms);
            }
            Ok(accepted)
        }
    }

    fn reject_request(
        &mut self,
        request_id: u32,
        rejection: Rejection,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        self.last_rejection_code = Some(rejection.code);
        Ok(self
            .protocol_mut()
            .reject_request(request_id, rejection.code, rejection.description)?)
    }

    /// Returns and clears the bounded protocol category for the latest rejected request.
    #[must_use]
    pub fn take_access_failure_code(&mut self) -> Option<&'static str> {
        self.last_rejection_code.take()
    }

    fn detach_role(&mut self, stream_id: u32, at_unix_ms: u64) -> Result<(), CatalogError> {
        self.last_callback_update_at_unix_ms.remove(&stream_id);
        let result = self.roles.remove(&stream_id).map_or(Ok(()), |mut role| {
            let result = role.release(at_unix_ms);
            self.notify_role_callbacks(&role);
            result
        });
        self.sync_control_state();
        result
    }

    fn detach_roles(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        let stream_ids: Vec<_> = self.roles.keys().copied().collect();
        let mut first_error = None;
        for stream_id in stream_ids {
            self.last_callback_update_at_unix_ms.remove(&stream_id);
            if let Some(mut role) = self.roles.remove(&stream_id) {
                let result = role.release(at_unix_ms);
                self.notify_role_callbacks(&role);
                if first_error.is_none() {
                    first_error = result.err();
                }
            }
        }
        self.sync_control_state();
        first_error.map_or(Ok(()), Err)
    }

    fn detach_role_if_matches(
        &mut self,
        stream_id: u32,
        application: &str,
        stream_name: &str,
        at_unix_ms: u64,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        if self.roles.contains_key(&stream_id) {
            if !self.role_matches(stream_id, application, stream_name) {
                return Err(RtmpSessionError::MessageStream { stream_id });
            }
            self.detach_role(stream_id, at_unix_ms)?;
        }
        Ok(Vec::new())
    }

    fn observe_role_at(&mut self, at_unix_ms: u64) -> Result<(), RtmpSessionError> {
        for role in self.roles.values_mut() {
            role.observe_at(at_unix_ms);
        }
        let identities: Vec<_> = self
            .roles
            .iter()
            .map(|(stream_id, role)| {
                let (application, stream_name) = role.identity();
                (*stream_id, application.to_owned(), stream_name.to_owned())
            })
            .collect();
        for (stream_id, application, stream_name) in identities {
            let service_policy = self.runtime.callbacks().clone();
            let application_policy = self
                .runtime
                .application(&application)
                .map(|app| app.callbacks().clone());
            let interval_ms = [Some(&service_policy), application_policy.as_ref()]
                .into_iter()
                .flatten()
                .filter(|policy| policy.has_update())
                .map(|policy| u64::try_from(policy.update_timeout.as_millis()).unwrap_or(u64::MAX))
                .min()
                .unwrap_or(0);
            let last_update_at = self
                .last_callback_update_at_unix_ms
                .get(&stream_id)
                .copied()
                .unwrap_or_default();
            if interval_ms > 0 && at_unix_ms.saturating_sub(last_update_at) >= interval_ms {
                self.last_callback_update_at_unix_ms
                    .insert(stream_id, at_unix_ms);
                let context = self.callback_context(&application, Some(&stream_name), None);
                Self::update_callbacks(&context, &service_policy, application_policy.as_ref())?;
            }
        }
        Ok(())
    }

    fn callback_context(
        &self,
        application: &str,
        stream_name: Option<&str>,
        query: Option<&str>,
    ) -> RtmpCallbackContext {
        RtmpCallbackContext {
            service_id: Arc::from(self.runtime.service_id()),
            application: Arc::from(application),
            stream_name: stream_name.map(Arc::from),
            query: query.map(Arc::from),
            client_addr: self.peer_addr,
            session_id: Arc::from(self.session_id.to_string()),
        }
    }

    fn notify_role_callbacks(&self, role: &SessionRole) {
        let (application, stream_name) = role.identity();
        let context = self.callback_context(application, Some(stream_name), None);
        let event = match role {
            SessionRole::Publisher(_) => RtmpCallbackEvent::PublishDone,
            SessionRole::Playback(_) | SessionRole::VodPlayback(_) => RtmpCallbackEvent::PlayDone,
        };
        self.notify_callbacks(application, event, &context);
        self.notify_callbacks(application, RtmpCallbackEvent::Done, &context);
    }

    fn authorize_callbacks(
        &self,
        application: &str,
        event: RtmpCallbackEvent,
        context: &RtmpCallbackContext,
    ) -> Result<(), RtmpCallbackError> {
        self.runtime.callbacks().authorize(event, context)?;
        if let Some(callbacks) = self
            .runtime
            .application(application)
            .map(RtmpApplication::callbacks)
        {
            callbacks.authorize(event, context)?;
        }
        Ok(())
    }

    fn notify_callbacks(
        &self,
        application: &str,
        event: RtmpCallbackEvent,
        context: &RtmpCallbackContext,
    ) {
        let _ = self.runtime.callbacks().notify(event, context);
        if let Some(callbacks) = self
            .runtime
            .application(application)
            .map(RtmpApplication::callbacks)
        {
            let _ = callbacks.notify(event, context);
        }
    }

    fn update_callbacks(
        context: &RtmpCallbackContext,
        service_policy: &RtmpCallbackPolicy,
        application_policy: Option<&RtmpCallbackPolicy>,
    ) -> Result<(), RtmpSessionError> {
        for policy in [Some(service_policy), application_policy]
            .into_iter()
            .flatten()
        {
            if policy.has_update() {
                if let Err(error) = policy.update(context) {
                    if policy.update_strict {
                        return Err(error.into());
                    }
                }
            }
        }
        Ok(())
    }

    fn role_matches(&self, stream_id: u32, application: &str, protocol_name: &str) -> bool {
        matches!(
            self.roles.get(&stream_id),
            Some(SessionRole::Publisher(publisher))
                if publisher.matches(application, protocol_name)
        ) || matches!(
            self.roles.get(&stream_id),
            Some(SessionRole::Playback(playback))
                if playback.matches(application, protocol_name)
        ) || matches!(
            self.roles.get(&stream_id),
            Some(SessionRole::VodPlayback(playback))
                if playback.matches(application, protocol_name)
        )
    }

    fn publisher_matches(&self, stream_id: u32, application: &str, protocol_name: &str) -> bool {
        matches!(
            self.roles.get(&stream_id),
            Some(SessionRole::Publisher(publisher))
                if publisher.matches(application, protocol_name)
        )
    }

    fn publisher_mut(&mut self, stream_id: u32) -> &mut publish::PublishSession {
        let Some(SessionRole::Publisher(publisher)) = self.roles.get_mut(&stream_id) else {
            unreachable!("matched publisher event requires an active publisher role");
        };
        publisher
    }

    fn protocol_mut(&mut self) -> &mut ServerSession {
        self.protocol
            .as_mut()
            .expect("server events require a protocol session")
    }

    pub(super) const fn peer_addr(&self) -> Option<IpAddr> {
        self.peer_addr
    }

    /// Returns and consumes a valid pending management disconnect request.
    #[must_use]
    pub fn take_control_action(&self) -> Option<RtmpSessionControlAction> {
        let message_streams = self.message_stream_snapshots();
        self.control
            .as_ref()
            .and_then(|control| control.take_action(&message_streams))
    }

    /// Returns the current management snapshot for this connection.
    #[must_use]
    pub fn client_snapshot(&self) -> Option<RtmpClientSnapshot> {
        self.control.as_ref().map(|control| control.snapshot())
    }

    fn message_stream_snapshots(&self) -> Vec<RtmpMessageStreamSnapshot> {
        self.roles
            .iter()
            .map(|(message_stream_id, role)| {
                let (application, stream_name) = role.identity();
                let role = match role {
                    SessionRole::Publisher(_) => RtmpSessionRole::Publisher,
                    SessionRole::Playback(_) | SessionRole::VodPlayback(_) => {
                        RtmpSessionRole::Subscriber
                    }
                };
                RtmpMessageStreamSnapshot {
                    message_stream_id: *message_stream_id,
                    application: application.to_owned(),
                    stream_name: stream_name.to_owned(),
                    role,
                }
            })
            .collect()
    }

    fn sync_control_state(&self) {
        let Some(control) = &self.control else {
            return;
        };
        let message_streams = self.message_stream_snapshots();
        control.set_message_streams(self.connected_application.as_deref(), &message_streams);
    }
}

impl Drop for RtmpSession {
    fn drop(&mut self) {
        // Recorder controllers must submit their workers before the session releases its reaper.
        for role in self.roles.values() {
            self.notify_role_callbacks(role);
        }
        if let Some(application) = self.connected_application.as_deref() {
            let context = self.callback_context(application, None, None);
            let _ = self
                .runtime
                .callbacks()
                .notify(RtmpCallbackEvent::Disconnect, &context);
            if let Some(callbacks) = self
                .runtime
                .application(application)
                .map(|application| application.callbacks().clone())
            {
                let _ = callbacks.notify(RtmpCallbackEvent::Disconnect, &context);
            }
        }
        self.roles.clear();
        self.runtime.registry().unregister_session(self.session_id);
    }
}

fn non_empty(bytes: Vec<u8>) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        Vec::new()
    } else {
        vec![bytes]
    }
}
