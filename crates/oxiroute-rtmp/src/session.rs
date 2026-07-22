use std::{collections::VecDeque, sync::Arc};

use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        PublishMode, ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
    },
};

use crate::{
    CatalogError, MediaSnapshot, RecorderDefinition, RtmpRegistry, SessionId, StreamId, StreamKey,
};

pub const MAX_INBOUND_CHUNK_SIZE: u32 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PublishSessionError {
    #[error("RTMP handshake failed: {0}")]
    Handshake(#[from] rml_rtmp::handshake::HandshakeError),
    #[error("RTMP session failed: {0}")]
    Session(#[from] rml_rtmp::sessions::ServerSessionError),
    #[error("RTMP catalog update failed: {0}")]
    Catalog(#[from] CatalogError),
    #[error("client chunk size {0} exceeds the {MAX_INBOUND_CHUNK_SIZE}-byte limit")]
    InboundChunkTooLarge(u32),
    #[error("RTMP media sample sequence exhausted")]
    MediaSequenceExhausted,
}

struct ActivePublisher {
    application: String,
    name: String,
    stream_id: StreamId,
}

/// Incremental server-side RTMP connection that accepts one live publisher.
pub struct RtmpPublishSession {
    server_id: String,
    registry: Arc<RtmpRegistry>,
    session_id: SessionId,
    handshake: Option<Handshake>,
    protocol: Option<ServerSession>,
    publisher: Option<ActivePublisher>,
    media: MediaSnapshot,
    media_sequence: u64,
}

impl RtmpPublishSession {
    #[must_use]
    pub fn new(server_id: impl Into<String>, registry: Arc<RtmpRegistry>) -> Self {
        Self {
            server_id: server_id.into(),
            registry,
            session_id: SessionId::new(),
            handshake: Some(Handshake::new(PeerType::Server)),
            protocol: None,
            publisher: None,
            media: MediaSnapshot::default(),
            media_sequence: 0,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Processes arbitrary contiguous bytes from this connection in wire order.
    ///
    /// # Errors
    ///
    /// Returns an error when the handshake, RTMP state machine, configured limits, or catalog
    /// transition fails. All returned packets must be written in order before reading more input.
    ///
    /// # Panics
    ///
    /// Panics only if the connection's internal handshake/session state invariant is broken.
    pub fn receive(
        &mut self,
        bytes: &[u8],
        at_unix_ms: u64,
    ) -> Result<Vec<Vec<u8>>, PublishSessionError> {
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
                let (protocol, startup) = ServerSession::new(ServerSessionConfig::new())?;
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

    /// Detaches this connection's publisher, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog no longer contains the publisher owned by this session.
    pub fn close(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        let Some(publisher) = &self.publisher else {
            return Ok(());
        };
        self.registry
            .detach_publisher(publisher.stream_id, self.session_id, at_unix_ms)?;
        self.publisher = None;
        Ok(())
    }

    fn receive_session_bytes(
        &mut self,
        bytes: &[u8],
        at_unix_ms: u64,
    ) -> Result<Vec<Vec<u8>>, PublishSessionError> {
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
    ) -> Result<Vec<Vec<u8>>, PublishSessionError> {
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
    ) -> Result<Vec<ServerSessionResult>, PublishSessionError> {
        match event {
            ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                Ok(self.protocol_mut().accept_request(request_id)?)
            }
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                app_name,
                stream_key,
                mode,
            } => self.handle_publish_request(request_id, app_name, stream_key, &mode, at_unix_ms),
            ServerSessionEvent::PublishStreamFinished {
                app_name,
                stream_key,
            } => {
                if self.publisher_matches(&app_name, &stream_key) {
                    self.close(at_unix_ms)?;
                }
                Ok(Vec::new())
            }
            ServerSessionEvent::PlayStreamRequested { request_id, .. } => {
                Ok(self.protocol_mut().reject_request(
                    request_id,
                    "NetStream.Play.Failed",
                    "playback is not implemented",
                )?)
            }
            ServerSessionEvent::ClientChunkSizeChanged { new_chunk_size }
                if new_chunk_size > MAX_INBOUND_CHUNK_SIZE =>
            {
                Err(PublishSessionError::InboundChunkTooLarge(new_chunk_size))
            }
            ServerSessionEvent::StreamMetadataChanged {
                app_name,
                stream_key,
                metadata,
            } if self.publisher_matches(&app_name, &stream_key) => {
                if let Some(codec) = metadata
                    .video_codec_id
                    .and_then(|codec| u8::try_from(codec).ok())
                {
                    self.media.video.flv_codec_id = Some(codec);
                }
                if let Some(codec) = metadata
                    .audio_codec_id
                    .and_then(|codec| u8::try_from(codec).ok())
                {
                    self.media.audio.flv_codec_id = Some(codec);
                }
                self.publish_media_sample(at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::AudioDataReceived {
                app_name,
                stream_key,
                data,
                timestamp,
            } if self.publisher_matches(&app_name, &stream_key) => {
                self.media.audio.flv_codec_id = data.first().map(|byte| byte >> 4);
                self.media.audio.payload_bytes_received = self
                    .media
                    .audio
                    .payload_bytes_received
                    .saturating_add(data.len() as u64);
                self.media.audio.last_rtmp_timestamp_ms = Some(timestamp.value);
                self.media.audio.last_observed_at_unix_ms = Some(at_unix_ms);
                self.publish_media_sample(at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::VideoDataReceived {
                app_name,
                stream_key,
                data,
                timestamp,
            } if self.publisher_matches(&app_name, &stream_key) => {
                self.media.video.flv_codec_id = data.first().map(|byte| byte & 0x0f);
                self.media.video.payload_bytes_received = self
                    .media
                    .video
                    .payload_bytes_received
                    .saturating_add(data.len() as u64);
                self.media.video.last_rtmp_timestamp_ms = Some(timestamp.value);
                self.media.video.last_observed_at_unix_ms = Some(at_unix_ms);
                self.publish_media_sample(at_unix_ms)?;
                Ok(Vec::new())
            }
            ServerSessionEvent::ClientChunkSizeChanged { .. }
            | ServerSessionEvent::ReleaseStreamRequested { .. }
            | ServerSessionEvent::StreamMetadataChanged { .. }
            | ServerSessionEvent::AudioDataReceived { .. }
            | ServerSessionEvent::VideoDataReceived { .. }
            | ServerSessionEvent::UnhandleableAmf0Command { .. }
            | ServerSessionEvent::PlayStreamFinished { .. }
            | ServerSessionEvent::AcknowledgementReceived { .. }
            | ServerSessionEvent::PingResponseReceived { .. } => Ok(Vec::new()),
        }
    }

    fn handle_publish_request(
        &mut self,
        request_id: u32,
        application: String,
        name: String,
        mode: &PublishMode,
        at_unix_ms: u64,
    ) -> Result<Vec<ServerSessionResult>, PublishSessionError> {
        if *mode != PublishMode::Live {
            return Ok(self.protocol_mut().reject_request(
                request_id,
                "NetStream.Publish.BadName",
                "only live publishing is supported",
            )?);
        }
        if self.publisher.is_some() {
            return Ok(self.protocol_mut().reject_request(
                request_id,
                "NetStream.Publish.BadName",
                "this connection is already publishing",
            )?);
        }

        let key = StreamKey::new(&self.server_id, &application, &name);
        let stream_id = match self.registry.attach_publisher(
            key,
            self.session_id,
            Vec::<RecorderDefinition>::new(),
            at_unix_ms,
        ) {
            Ok(stream_id) => stream_id,
            Err(CatalogError::PublisherAlreadyAttached { .. }) => {
                return Ok(self.protocol_mut().reject_request(
                    request_id,
                    "NetStream.Publish.BadName",
                    "stream already has a publisher",
                )?);
            }
            Err(error) => return Err(error.into()),
        };

        let accepted = match self.protocol_mut().accept_request(request_id) {
            Ok(results) => results,
            Err(error) => {
                self.registry
                    .detach_publisher(stream_id, self.session_id, at_unix_ms)?;
                return Err(error.into());
            }
        };
        self.publisher = Some(ActivePublisher {
            application,
            name,
            stream_id,
        });
        self.media = MediaSnapshot::default();
        self.media_sequence = 0;
        Ok(accepted)
    }

    fn publish_media_sample(&mut self, at_unix_ms: u64) -> Result<(), PublishSessionError> {
        let stream_id = self
            .publisher
            .as_ref()
            .expect("media events require an active publisher")
            .stream_id;
        self.media_sequence = self
            .media_sequence
            .checked_add(1)
            .ok_or(PublishSessionError::MediaSequenceExhausted)?;
        self.registry.update_media_sample(
            stream_id,
            self.session_id,
            self.media_sequence,
            self.media,
            at_unix_ms,
        )?;
        Ok(())
    }

    fn publisher_matches(&self, application: &str, name: &str) -> bool {
        self.publisher
            .as_ref()
            .is_some_and(|publisher| publisher.application == application && publisher.name == name)
    }

    fn protocol_mut(&mut self) -> &mut ServerSession {
        self.protocol
            .as_mut()
            .expect("server events require a protocol session")
    }
}

fn non_empty(bytes: Vec<u8>) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        Vec::new()
    } else {
        vec![bytes]
    }
}
