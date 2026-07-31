use crate::{CatalogError, LiveHubError, MediaEventError, RtmpStreamPathError};

use super::{
    MAX_INBOUND_CHUNK_SIZE,
    runtime::{PlaybackRoleError, PublisherRoleError},
};

pub(super) const CONNECT_REJECTION_CODE: &str = "NetConnection.Connect.Rejected";
pub(super) const PUBLISH_REJECTION_CODE: &str = "NetStream.Publish.BadName";
pub(super) const PLAY_REJECTION_CODE: &str = "NetStream.Play.Failed";
pub(super) const PLAY_NOT_FOUND_CODE: &str = "NetStream.Play.StreamNotFound";

#[derive(Clone, Copy)]
pub(super) struct Rejection {
    pub(super) code: &'static str,
    pub(super) description: &'static str,
}

impl Rejection {
    pub(super) const fn new(code: &'static str, description: &'static str) -> Self {
        Self { code, description }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RtmpSessionError {
    #[error("RTMP handshake failed: {0}")]
    Handshake(#[from] rml_rtmp::handshake::HandshakeError),
    #[error("RTMP session failed: {0}")]
    Session(#[from] rml_rtmp::sessions::ServerSessionError),
    #[error("RTMP catalog update failed: {0}")]
    Catalog(#[from] CatalogError),
    #[error("RTMP live fanout failed: {0}")]
    LiveHub(#[from] LiveHubError),
    #[error("RTMP media event is invalid: {0}")]
    MediaEvent(#[from] MediaEventError),
    #[error("client chunk size {0} exceeds the {MAX_INBOUND_CHUNK_SIZE}-byte limit")]
    InboundChunkTooLarge(u32),
    #[error("RTMP media sample sequence exhausted")]
    MediaSequenceExhausted,
    #[error("queued metadata event has no structured RTMP metadata")]
    MissingMetadata,
    #[error("RTMP playback drain requires an active live playback role")]
    NoActivePlayback,
}

pub(super) fn connection_path(error: RtmpStreamPathError) -> Rejection {
    match error {
        RtmpStreamPathError::ApplicationTooLong { .. } => Rejection::new(
            CONNECT_REJECTION_CODE,
            "RTMP application exceeds the configured byte limit",
        ),
        RtmpStreamPathError::Application
        | RtmpStreamPathError::StreamName
        | RtmpStreamPathError::Query
        | RtmpStreamPathError::StreamNameTooLong { .. }
        | RtmpStreamPathError::QueryTooLong { .. } => {
            Rejection::new(CONNECT_REJECTION_CODE, "RTMP application path is invalid")
        }
    }
}

pub(super) fn publish_path(error: RtmpStreamPathError) -> Rejection {
    Rejection::new(PUBLISH_REJECTION_CODE, stream_path_description(error))
}

pub(super) fn playback_path(error: RtmpStreamPathError) -> Rejection {
    Rejection::new(PLAY_REJECTION_CODE, stream_path_description(error))
}

pub(super) fn publisher_role(error: PublisherRoleError) -> Result<Rejection, RtmpSessionError> {
    match error {
        PublisherRoleError::AdmissionClosed => Ok(Rejection::new(
            PUBLISH_REJECTION_CODE,
            "RTMP runtime is shutting down",
        )),
        PublisherRoleError::Hub(LiveHubError::PublisherAlreadyAttached { .. })
        | PublisherRoleError::Catalog(CatalogError::PublisherAlreadyAttached { .. }) => {
            Ok(Rejection::new(
                PUBLISH_REJECTION_CODE,
                "stream already has a publisher; RTMP queries are non-identity data",
            ))
        }
        PublisherRoleError::Hub(
            LiveHubError::StreamLimitReached { .. } | LiveHubError::IdentityExhausted,
        ) => Ok(Rejection::new(
            PUBLISH_REJECTION_CODE,
            "stream publisher capacity is unavailable",
        )),
        PublisherRoleError::Hub(error) => Err(error.into()),
        PublisherRoleError::Catalog(error) => Err(error.into()),
    }
}

pub(super) fn playback_role(error: PlaybackRoleError) -> Result<Rejection, RtmpSessionError> {
    match error {
        PlaybackRoleError::AdmissionClosed => Ok(Rejection::new(
            PLAY_REJECTION_CODE,
            "RTMP runtime is shutting down",
        )),
        PlaybackRoleError::NoPublisher => Ok(Rejection::new(
            PLAY_NOT_FOUND_CODE,
            "live stream has no publisher",
        )),
        PlaybackRoleError::Hub(
            LiveHubError::StreamLimitReached { .. }
            | LiveHubError::SubscriberLimitReached { .. }
            | LiveHubError::StreamSubscriberLimitReached { .. }
            | LiveHubError::IdentityExhausted,
        ) => Ok(Rejection::new(
            PLAY_REJECTION_CODE,
            "playback capacity is unavailable",
        )),
        PlaybackRoleError::Hub(error) => Err(error.into()),
        PlaybackRoleError::Catalog(error) => Err(error.into()),
    }
}

const fn stream_path_description(error: RtmpStreamPathError) -> &'static str {
    match error {
        RtmpStreamPathError::ApplicationTooLong { .. }
        | RtmpStreamPathError::StreamNameTooLong { .. }
        | RtmpStreamPathError::QueryTooLong { .. } => {
            "RTMP stream identity exceeds the configured byte limit"
        }
        RtmpStreamPathError::Application
        | RtmpStreamPathError::StreamName
        | RtmpStreamPathError::Query => "invalid RTMP application or stream path",
    }
}
