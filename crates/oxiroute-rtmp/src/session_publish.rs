use std::sync::Arc;

use bytes::Bytes;
use rml_rtmp::{
    sessions::{PublishMode, ServerSessionResult, StreamMetadata},
    time::RtmpTimestamp,
};

use crate::{
    CatalogError, LiveHub, MediaEvent, MediaSnapshot, PublisherLease, PublisherRegistration,
    RtmpRegistry, SessionId, StreamId, StreamKey, VideoCodecIdentifier,
    recording_runtime::RecorderController, relay::RtmpRelayController,
};

use super::{
    RtmpSession,
    identity::{self, StreamIdentity},
    runtime::{RTMP_STALE_PUBLISHER_THRESHOLD_MS, SessionRole},
    status::{self, PUBLISH_REJECTION_CODE, Rejection, RtmpSessionError},
};

pub(super) struct PublishSession {
    key: StreamKey,
    hub: LiveHub,
    lease: Option<PublisherLease>,
    registration: Option<PublisherRegistration>,
    registry: Arc<RtmpRegistry>,
    session_id: SessionId,
    media: MediaSnapshot,
    media_sequence: u64,
    last_media_activity_at_unix_ms: u64,
    recorders: Vec<(crate::RecorderId, Arc<RecorderController>)>,
    relays: Vec<Arc<RtmpRelayController>>,
}

pub(super) struct PublisherOutputs {
    pub recorders: Vec<(crate::RecorderId, Arc<RecorderController>)>,
    pub relays: Vec<Arc<RtmpRelayController>>,
}

impl PublishSession {
    pub(super) fn new(
        key: StreamKey,
        hub: LiveHub,
        lease: PublisherLease,
        registration: PublisherRegistration,
        registry: Arc<RtmpRegistry>,
        session_id: SessionId,
        outputs: PublisherOutputs,
    ) -> Self {
        let PublisherOutputs { recorders, relays } = outputs;
        let last_media_activity_at_unix_ms = registration.last_observed_at_unix_ms();
        Self {
            key,
            hub,
            lease: Some(lease),
            registration: Some(registration),
            registry,
            session_id,
            media: MediaSnapshot::default(),
            media_sequence: 0,
            last_media_activity_at_unix_ms,
            recorders,
            relays,
        }
    }

    pub(super) fn matches(&self, application: &str, protocol_name: &str) -> bool {
        identity::matches(&self.key, application, protocol_name)
    }

    pub(super) fn handle_metadata(
        &mut self,
        metadata: StreamMetadata,
        at_unix_ms: u64,
    ) -> Result<(), RtmpSessionError> {
        let video_codec = metadata
            .video_codec_id
            .and_then(|codec| u8::try_from(codec).ok());
        self.media.video.flv_codec_id = video_codec;
        self.media.video.video_codec = video_codec.map(VideoCodecIdentifier::Flv);
        self.media.audio.flv_codec_id = metadata
            .audio_codec_id
            .and_then(|codec| u8::try_from(codec).ok());
        self.last_media_activity_at_unix_ms = self.last_media_activity_at_unix_ms.max(at_unix_ms);
        self.publish_event(&MediaEvent::metadata(metadata)?, at_unix_ms)
    }

    pub(super) fn handle_audio(
        &mut self,
        data: &Bytes,
        timestamp: RtmpTimestamp,
        at_unix_ms: u64,
    ) -> Result<(), RtmpSessionError> {
        let event = MediaEvent::audio(timestamp.value, Arc::<[u8]>::from(data.as_ref()))?;
        self.media.audio.flv_codec_id = data.first().map(|byte| byte >> 4);
        self.media.audio.payload_bytes_received = self
            .media
            .audio
            .payload_bytes_received
            .saturating_add(data.len() as u64);
        self.media.audio.last_rtmp_timestamp_ms = Some(timestamp.value);
        self.media.audio.last_observed_at_unix_ms = Some(at_unix_ms);
        self.last_media_activity_at_unix_ms = self.last_media_activity_at_unix_ms.max(at_unix_ms);
        self.publish_event(&event, at_unix_ms)
    }

    pub(super) fn handle_video(
        &mut self,
        data: &Bytes,
        timestamp: RtmpTimestamp,
        at_unix_ms: u64,
    ) -> Result<(), RtmpSessionError> {
        let event = MediaEvent::video(timestamp.value, Arc::<[u8]>::from(data.as_ref()))?;
        self.media.video.flv_codec_id = event
            .video_codec_identifier()
            .and_then(VideoCodecIdentifier::flv_codec_id);
        self.media.video.video_codec = event.video_codec_identifier();
        self.media.video.payload_bytes_received = self
            .media
            .video
            .payload_bytes_received
            .saturating_add(data.len() as u64);
        self.media.video.last_rtmp_timestamp_ms = Some(timestamp.value);
        self.media.video.last_observed_at_unix_ms = Some(at_unix_ms);
        self.last_media_activity_at_unix_ms = self.last_media_activity_at_unix_ms.max(at_unix_ms);
        self.publish_event(&event, at_unix_ms)
    }

    pub(super) fn is_stale(&self, at_unix_ms: u64) -> bool {
        at_unix_ms.saturating_sub(self.last_media_activity_at_unix_ms)
            >= RTMP_STALE_PUBLISHER_THRESHOLD_MS
    }

    pub(super) fn observe_at(&mut self, at_unix_ms: u64) {
        if let Some(registration) = &mut self.registration {
            registration.observe_at(at_unix_ms);
        }
    }

    pub(super) fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        let hub = self.hub.clone();
        let shutdown = {
            let _transaction = hub.lock_roles();
            let shutdown = self
                .registration
                .take()
                .map_or(Ok(None), |mut registration| {
                    registration.observe_at(at_unix_ms);
                    registration.release_deferred(at_unix_ms).map(Some)
                })?;
            self.lease.take();
            shutdown
        };
        if let Some(shutdown) = shutdown {
            shutdown.shutdown(at_unix_ms);
        }
        Ok(())
    }

    fn stream_id(&self) -> StreamId {
        self.registration
            .as_ref()
            .expect("active publisher owns a catalog registration")
            .stream_id()
    }

    fn publish_event(
        &mut self,
        event: &MediaEvent,
        at_unix_ms: u64,
    ) -> Result<(), RtmpSessionError> {
        let stream_id = self.stream_id();
        let report = self
            .lease
            .as_ref()
            .expect("active publisher owns a fanout lease")
            .publish(event.clone())?;
        self.media.fanout_payload_bytes_queued =
            u64::try_from(report.stream_fanout_bytes).unwrap_or(u64::MAX);
        self.media_sequence = self
            .media_sequence
            .checked_add(1)
            .ok_or(RtmpSessionError::MediaSequenceExhausted)?;
        for (recorder_id, controller) in &self.recorders {
            let result = controller.try_enqueue(event.clone(), at_unix_ms);
            self.registry.update_recorder_runtime(
                stream_id,
                self.session_id,
                *recorder_id,
                result,
                at_unix_ms,
            );
        }
        for relay in &self.relays {
            relay.try_enqueue(event.clone());
        }
        self.registry.update_media_sample(
            stream_id,
            self.session_id,
            self.media_sequence,
            self.media,
            at_unix_ms,
        )?;
        Ok(())
    }
}

impl Drop for PublishSession {
    fn drop(&mut self) {
        if self.registration.is_none() && self.lease.is_none() {
            return;
        }
        let hub = self.hub.clone();
        let (shutdown, shutdown_at) = {
            let _transaction = hub.lock_roles();
            let (shutdown, shutdown_at) =
                self.registration
                    .take()
                    .map_or((None, 0), |mut registration| {
                        let at_unix_ms = registration.last_observed_at_unix_ms();
                        (registration.release_deferred(at_unix_ms).ok(), at_unix_ms)
                    });
            self.lease.take();
            (shutdown, shutdown_at)
        };
        if let Some(shutdown) = shutdown {
            shutdown.shutdown(shutdown_at);
        }
    }
}

pub(super) fn handle_request(
    session: &mut RtmpSession,
    request_id: u32,
    application: &str,
    protocol_name: &str,
    mode: &PublishMode,
    at_unix_ms: u64,
) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
    let identity =
        match StreamIdentity::parse(session.runtime.service_id(), application, protocol_name) {
            Ok(identity) => identity,
            Err(error) => {
                return session.reject_request(request_id, status::publish_path(error));
            }
        };
    let live_enabled = session
        .runtime
        .application(application)
        .is_some_and(super::RtmpApplication::live);
    if *mode != PublishMode::Live || !live_enabled {
        return session.reject_request(
            request_id,
            Rejection::new(
                PUBLISH_REJECTION_CODE,
                "live publishing is not enabled for this application",
            ),
        );
    }
    if session.role.is_some() {
        return session.reject_request(
            request_id,
            Rejection::new(
                PUBLISH_REJECTION_CODE,
                "this connection already has an active media role",
            ),
        );
    }

    let mut role = match session.runtime.acquire_publisher_role(
        identity.into_key(),
        session.session_id,
        at_unix_ms,
    ) {
        Ok(role) => role,
        Err(error) => {
            let rejection = status::publisher_role(error)?;
            return session.reject_request(request_id, rejection);
        }
    };

    let accepted = match session.protocol_mut().accept_request(request_id) {
        Ok(results) => results,
        Err(error) => {
            role.release(at_unix_ms)?;
            return Err(error.into());
        }
    };
    session.role = Some(SessionRole::Publisher(role));
    Ok(accepted)
}
