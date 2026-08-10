use bytes::Bytes;
use rml_rtmp::{
    sessions::{ServerSession, ServerSessionResult},
    time::RtmpTimestamp,
};

use crate::{
    CatalogError, LiveHub, MediaEvent, MediaEventKind, PlaybackSubscription, RtmpCallbackEvent,
    StreamKey, SubscriberRegistration,
};

use super::{
    AdmissionTransaction, RtmpSession,
    identity::{self, StreamIdentity, VodIdentity},
    runtime::{SessionOperation, SessionRole, drop_role, release_role},
    status::{self, PLAY_NOT_FOUND_CODE, PLAY_REJECTION_CODE, Rejection, RtmpSessionError},
};

pub const MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN: usize = 32;

pub(super) struct PlaybackSession {
    key: StreamKey,
    hub: LiveHub,
    protocol_stream_id: u32,
    subscription: Option<PlaybackSubscription>,
    registration: Option<SubscriberRegistration>,
    _session_lease: super::runtime::ApplicationSessionLease,
}

impl PlaybackSession {
    pub(super) fn new(
        key: StreamKey,
        hub: LiveHub,
        protocol_stream_id: u32,
        subscription: PlaybackSubscription,
        registration: SubscriberRegistration,
        session_lease: super::runtime::ApplicationSessionLease,
    ) -> Self {
        Self {
            key,
            hub,
            protocol_stream_id,
            subscription: Some(subscription),
            registration: Some(registration),
            _session_lease: session_lease,
        }
    }

    pub(super) fn matches(&self, application: &str, protocol_name: &str) -> bool {
        identity::matches(&self.key, application, protocol_name)
    }

    pub(super) fn application(&self) -> &str {
        &self.key.application
    }

    pub(super) fn stream_name(&self) -> &str {
        &self.key.name
    }

    pub(super) fn observe_at(&mut self, at_unix_ms: u64) {
        if let Some(registration) = &mut self.registration {
            registration.observe_at(at_unix_ms);
        }
    }

    pub(super) fn release(&mut self, at_unix_ms: u64) -> Result<(), CatalogError> {
        release_role(
            &self.hub,
            &mut self.registration,
            &mut self.subscription,
            at_unix_ms,
            |registration, at_unix_ms| {
                registration.observe_at(at_unix_ms);
                registration.release(at_unix_ms)
            },
        )
    }

    fn take_events(&self, maximum_events: usize) -> Vec<MediaEvent> {
        let event_limit = maximum_events.min(MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN);
        let subscription = self
            .subscription
            .as_ref()
            .expect("active playback owns a fanout subscription");
        (0..event_limit)
            .map_while(|_| subscription.try_next())
            .collect()
    }
}

impl Drop for PlaybackSession {
    fn drop(&mut self) {
        drop_role(&self.hub, &mut self.registration, &mut self.subscription);
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn handle_request(
    session: &mut RtmpSession,
    request_id: u32,
    application: &str,
    protocol_name: &str,
    protocol_stream_id: u32,
    at_unix_ms: u64,
) -> Result<Vec<ServerSessionResult>, RtmpSessionError> {
    if let Some(vod_identity) = VodIdentity::parse(protocol_name) {
        if session.runtime.application(application).is_none() {
            return session.reject_request(
                request_id,
                Rejection::new(PLAY_NOT_FOUND_CODE, "RTMP application is not configured"),
            );
        }
        if session.connected_application.as_deref() != Some(application) {
            return session.reject_request(
                request_id,
                Rejection::new(
                    PLAY_REJECTION_CODE,
                    "RTMP application does not match the connected application",
                ),
            );
        }
        if let Err(error) = session.runtime.authorize(
            application,
            SessionOperation::Play,
            session.peer_addr(),
            vod_identity.query(),
        ) {
            return session.reject_request(
                request_id,
                status::authorization(SessionOperation::Play, error),
            );
        }
        if session.roles.contains_key(&protocol_stream_id) {
            return session.reject_request(
                request_id,
                Rejection::new(
                    PLAY_REJECTION_CODE,
                    "this connection already has an active media role",
                ),
            );
        }
        if session.roles.len() >= super::MAX_RTMP_MESSAGE_STREAMS {
            return session.reject_request(
                request_id,
                Rejection::new(
                    PLAY_REJECTION_CODE,
                    "RTMP message stream limit reached for this connection",
                ),
            );
        }
        let stream_name = protocol_name
            .split('?')
            .next()
            .unwrap_or(protocol_name)
            .to_owned();
        let role = match session.runtime.acquire_vod_playback(
            application,
            vod_identity.source(),
            vod_identity.path(),
            stream_name,
            protocol_stream_id,
        ) {
            Ok(role) => role,
            Err(error) => {
                let rejection = status::playback_role(error)?;
                return session.reject_request(request_id, rejection);
            }
        };
        return AdmissionTransaction::new(
            session,
            request_id,
            protocol_stream_id,
            at_unix_ms,
            SessionRole::VodPlayback(role),
        )
        .commit();
    }
    let identity =
        match StreamIdentity::parse(session.runtime.service_id(), application, protocol_name) {
            Ok(identity) => identity,
            Err(error) => {
                return session.reject_request(request_id, status::playback_path(error));
            }
        };
    let Some(application_policy) = session.runtime.application(application) else {
        return session.reject_request(
            request_id,
            Rejection::new(PLAY_NOT_FOUND_CODE, "RTMP application is not configured"),
        );
    };
    if session.connected_application.as_deref() != Some(application) {
        return session.reject_request(
            request_id,
            Rejection::new(
                PLAY_REJECTION_CODE,
                "RTMP application does not match the connected application",
            ),
        );
    }
    if !application_policy.live() {
        return session.reject_request(
            request_id,
            Rejection::new(PLAY_NOT_FOUND_CODE, "only live playback is available"),
        );
    }
    if let Err(error) = session.runtime.authorize(
        application,
        SessionOperation::Play,
        session.peer_addr(),
        identity.query(),
    ) {
        return session.reject_request(
            request_id,
            status::authorization(SessionOperation::Play, error),
        );
    }
    if session.roles.contains_key(&protocol_stream_id) {
        return session.reject_request(
            request_id,
            Rejection::new(
                PLAY_REJECTION_CODE,
                "this connection already has an active media role",
            ),
        );
    }
    if session.roles.len() >= super::MAX_RTMP_MESSAGE_STREAMS {
        return session.reject_request(
            request_id,
            Rejection::new(
                PLAY_REJECTION_CODE,
                "RTMP message stream limit reached for this connection",
            ),
        );
    }
    let context =
        session.callback_context(application, Some(identity.stream_name()), identity.query());
    if let Err(error) = session.authorize_callbacks(application, RtmpCallbackEvent::Play, &context)
    {
        return session.reject_request(request_id, status::callback(SessionOperation::Play, error));
    }

    let idle_streams = application_policy.idle_streams();
    let role = match session.runtime.acquire_playback_role(
        identity.into_key(),
        session.session_id,
        protocol_stream_id,
        idle_streams,
        at_unix_ms,
    ) {
        Ok(role) => role,
        Err(error) => {
            let rejection = status::playback_role(error)?;
            return session.reject_request(request_id, rejection);
        }
    };
    AdmissionTransaction::new(
        session,
        request_id,
        protocol_stream_id,
        at_unix_ms,
        SessionRole::Playback(role),
    )
    .commit()
}

pub(super) fn drain(
    session: &mut RtmpSession,
    maximum_events: usize,
) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
    if !session.is_playback_active() {
        return Err(RtmpSessionError::NoActivePlayback);
    }
    if maximum_events == 0 {
        return Ok(Vec::new());
    }

    let stream_ids: Vec<_> = session.roles.keys().copied().collect();
    let mut outbound = Vec::new();
    for stream_id in stream_ids {
        if outbound.len() >= maximum_events {
            break;
        }
        let Some(role) = session.roles.get_mut(&stream_id) else {
            continue;
        };
        let remaining = maximum_events - outbound.len();
        let packets = match role {
            SessionRole::Playback(playback) => {
                let events = playback.take_events(remaining);
                let protocol = session
                    .protocol
                    .as_mut()
                    .expect("playback drain requires a protocol session");
                events
                    .into_iter()
                    .map(|event| serialize_event(protocol, playback.protocol_stream_id, &event))
                    .collect::<Result<Vec<_>, _>>()
            }
            SessionRole::VodPlayback(playback) => {
                let protocol = session
                    .protocol
                    .as_mut()
                    .expect("VOD playback drain requires a protocol session");
                playback.drain(protocol, remaining)
            }
            SessionRole::Publisher(_) => Ok(Vec::new()),
        }?;
        outbound.extend(packets);
    }
    Ok(outbound)
}

pub(super) fn serialize_event(
    protocol: &mut ServerSession,
    stream_id: u32,
    event: &MediaEvent,
) -> Result<Vec<u8>, RtmpSessionError> {
    let packet = match event.kind() {
        MediaEventKind::Metadata => protocol.send_metadata(
            stream_id,
            event
                .stream_metadata()
                .ok_or(RtmpSessionError::MissingMetadata)?,
        )?,
        MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => protocol.send_audio_data(
            stream_id,
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            false,
        )?,
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => protocol.send_video_data(
            stream_id,
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            event.kind() == MediaEventKind::VideoDisposable,
        )?,
    };
    Ok(packet.bytes)
}
