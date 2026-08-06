use std::{collections::VecDeque, net::IpAddr, sync::Arc};

use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult},
};

use crate::{
    CatalogError, LiveHub, RtmpCallbackContext, RtmpCallbackError, RtmpCallbackEvent,
    RtmpCallbackPolicy, RtmpClientSnapshot, RtmpRegistry, RtmpSessionControlAction,
    RtmpSessionRole, SessionId,
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

use runtime::SessionRole;
use crate::session_control::RtmpSessionControl;
use status::{CONNECT_REJECTION_CODE, Rejection};

pub const MAX_INBOUND_CHUNK_SIZE: u32 = 1024 * 1024;
pub const MAX_INBOUND_MESSAGE_SIZE: usize = 8 * 1024 * 1024;
pub const MAX_INBOUND_AMF0_DEPTH: usize = 32;
pub const MAX_INBOUND_AMF0_CONTAINER_ENTRIES: usize = 1_024;
pub const MAX_INBOUND_AMF0_VALUES: usize = 4_096;
pub const MAX_INBOUND_AMF0_STRING_BYTES: usize = u16::MAX as usize;

/// Incremental server-side RTMP connection with one live publisher or playback role.
pub struct RtmpSession {
    runtime: RtmpServiceRuntime,
    session_id: SessionId,
    handshake: Option<Handshake>,
    protocol: Option<ServerSession>,
    role: Option<SessionRole>,
    peer_addr: Option<IpAddr>,
    connection_lease: Option<runtime::ApplicationSessionLease>,
    connected_application: Option<Arc<str>>,
    control: Option<Arc<RtmpSessionControl>>,
    last_callback_update_at_unix_ms: u64,
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
        let control = runtime.registry().register_session(
            session_id,
            runtime.service_id(),
            peer_addr,
        );
        Self {
            runtime,
            session_id,
            handshake: Some(Handshake::new(PeerType::Server)),
            protocol: None,
            role: None,
            peer_addr,
            connection_lease: None,
            connected_application: None,
            control,
            last_callback_update_at_unix_ms: 0,
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
        matches!(
            self.role,
            Some(SessionRole::Playback(_) | SessionRole::VodPlayback(_))
        )
    }

    /// Returns whether the active publisher has emitted no metadata or media for the stale
    /// publisher threshold.
    #[must_use]
    pub fn is_publisher_stale(&self, at_unix_ms: u64) -> bool {
        matches!(
            &self.role,
            Some(SessionRole::Publisher(publisher)) if publisher.is_stale(at_unix_ms)
        )
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

    pub(super) fn drain_vod_playback(
        &mut self,
        maximum_events: usize,
    ) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
        let Some(SessionRole::VodPlayback(mut playback)) = self.role.take() else {
            return Err(RtmpSessionError::NoActivePlayback);
        };
        let result = playback.drain(self.protocol_mut(), maximum_events);
        self.role = Some(SessionRole::VodPlayback(playback));
        result
    }

    /// Detaches this connection's active publisher or viewer, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog no longer contains the role owned by this session.
    pub fn close(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        let result = self.detach_role(at_unix_ms);
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
            } => {
                let result =
                    publish::handle_request(self, request_id, &app_name, &stream_key, &mode, at_unix_ms);
                if result.is_ok() {
                    self.sync_control_state();
                }
                result
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
                if new_chunk_size > self.runtime.inbound_limits().max_inbound_chunk_size =>
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
        at_unix_ms: u64,
    ) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
        if let Err(error) = identity::validate_application(application) {
            return self.reject_request(request_id, status::connection_path(error));
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
        Ok(self
            .protocol_mut()
            .reject_request(request_id, rejection.code, rejection.description)?)
    }

    fn detach_role(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        let result = self.role.take().map_or(Ok(()), |mut role| {
            let result = role.release(at_unix_ms);
            self.notify_role_callbacks(&role);
            result
        });
        self.sync_control_state();
        result
    }

    fn observe_role_at(&mut self, at_unix_ms: u64) -> Result<(), RtmpSessionError> {
        let identity = self
            .role
            .as_ref()
            .map(|role| (role.identity().0.to_owned(), role.identity().1.to_owned()));
        if let Some((application, stream_name)) = identity {
            if let Some(role) = &mut self.role {
                role.observe_at(at_unix_ms);
            }
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
            if interval_ms > 0
                && at_unix_ms.saturating_sub(self.last_callback_update_at_unix_ms) >= interval_ms
            {
                self.last_callback_update_at_unix_ms = at_unix_ms;
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

    fn publisher_matches(&self, application: &str, protocol_name: &str) -> bool {
        matches!(
            &self.role,
            Some(SessionRole::Publisher(publisher))
                if publisher.matches(application, protocol_name)
        )
    }

    fn playback_matches(&self, application: &str, protocol_name: &str) -> bool {
        match &self.role {
            Some(SessionRole::Playback(playback)) => playback.matches(application, protocol_name),
            Some(SessionRole::VodPlayback(playback)) => {
                playback.matches(application, protocol_name)
            }
            Some(SessionRole::Publisher(_)) | None => false,
        }
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

    pub(super) const fn peer_addr(&self) -> Option<IpAddr> {
        self.peer_addr
    }

    /// Returns and consumes a valid pending management disconnect request.
    #[must_use]
    pub fn take_control_action(&self) -> Option<RtmpSessionControlAction> {
        self.control
            .as_ref()
            .and_then(|control| control.take_action(self.control_role()))
    }

    /// Returns the current management snapshot for this connection.
    #[must_use]
    pub fn client_snapshot(&self) -> Option<RtmpClientSnapshot> {
        self.control.as_ref().map(|control| control.snapshot())
    }

    fn control_role(&self) -> RtmpSessionRole {
        match self.role {
            Some(SessionRole::Publisher(_)) => RtmpSessionRole::Publisher,
            Some(SessionRole::Playback(_) | SessionRole::VodPlayback(_)) => {
                RtmpSessionRole::Subscriber
            }
            None => RtmpSessionRole::Client,
        }
    }

    fn sync_control_state(&self) {
        let Some(control) = &self.control else {
            return;
        };
        let (application, stream_name) = self
            .role
            .as_ref()
            .map(SessionRole::identity)
            .map_or((self.connected_application.as_deref(), None), |(application, stream_name)| {
                (Some(application), Some(stream_name))
            });
        control.set_role(self.control_role(), application, stream_name);
    }
}

impl Drop for RtmpSession {
    fn drop(&mut self) {
        // Recorder controllers must submit their workers before the session releases its reaper.
        if let Some(role) = self.role.as_ref() {
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
        self.role.take();
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
