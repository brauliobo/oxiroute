use oxiroute_rtmp::{
    RecorderErrorCode, RecorderNotification, RtmpDnsRefreshFailure, RtmpRelayFailure,
    RtmpRelayPhase, RtmpSessionControlAction, RtmpSessionRole,
};
use serde::Serialize;

use super::*;

fn names<T: Serialize>(values: impl IntoIterator<Item = T>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| {
            serde_json::to_value(value)
                .expect("wire enum JSON")
                .as_str()
                .expect("wire enum string")
                .to_owned()
        })
        .collect()
}

#[test]
fn relay_enums_cover_every_domain_variant() {
    assert_eq!(
        names(
            [
                RtmpRelayPhase::Connecting,
                RtmpRelayPhase::Publishing,
                RtmpRelayPhase::Pulling,
                RtmpRelayPhase::Backoff,
                RtmpRelayPhase::Stopped,
            ]
            .map(RelayPhaseDto::from)
        ),
        ["connecting", "publishing", "pulling", "backoff", "stopped"]
    );
    assert_eq!(
        names(
            [
                RtmpRelayFailure::Policy,
                RtmpRelayFailure::Connect,
                RtmpRelayFailure::Handshake,
                RtmpRelayFailure::Session,
                RtmpRelayFailure::Transport,
                RtmpRelayFailure::Source,
                RtmpRelayFailure::Thread,
            ]
            .map(RelayFailureDto::from)
        ),
        [
            "policy",
            "connect",
            "handshake",
            "session",
            "transport",
            "source",
            "thread"
        ]
    );
    assert_eq!(
        names(
            [
                RtmpDnsRefreshFailure::Resolution,
                RtmpDnsRefreshFailure::AddressSet,
                RtmpDnsRefreshFailure::Policy,
                RtmpDnsRefreshFailure::DirectLoop,
                RtmpDnsRefreshFailure::FamilyMismatch,
            ]
            .map(RelayDnsRefreshFailureDto::from)
        ),
        [
            "resolution",
            "address_set",
            "policy",
            "direct_loop",
            "family_mismatch"
        ]
    );
}

#[test]
fn recorder_and_session_enums_cover_every_domain_variant() {
    assert_eq!(
        names(
            [
                RecorderNotification::Started,
                RecorderNotification::Stopped,
                RecorderNotification::Failed,
            ]
            .map(RecorderNotificationDto::from)
        ),
        ["started", "stopped", "failed"]
    );
    assert_eq!(
        names(
            [
                RecorderErrorCode::OpenFailed,
                RecorderErrorCode::WriteFailed,
                RecorderErrorCode::CloseFailed,
                RecorderErrorCode::BackendUnavailable,
                RecorderErrorCode::FileSyncFailed,
                RecorderErrorCode::PublishFailed,
                RecorderErrorCode::DirectorySyncFailed,
                RecorderErrorCode::QueueDiscontinuity,
                RecorderErrorCode::UnsupportedCodec,
                RecorderErrorCode::ShutdownTimedOut,
                RecorderErrorCode::WorkerPanicked,
                RecorderErrorCode::StalePublisher,
            ]
            .map(RecorderErrorCodeDto::from)
        ),
        [
            "open_failed",
            "write_failed",
            "close_failed",
            "backend_unavailable",
            "file_sync_failed",
            "publish_failed",
            "directory_sync_failed",
            "queue_discontinuity",
            "unsupported_codec",
            "shutdown_timed_out",
            "worker_panicked",
            "stale_publisher",
        ]
    );
    assert_eq!(
        names(
            [
                RtmpSessionRole::Client,
                RtmpSessionRole::Publisher,
                RtmpSessionRole::Subscriber,
            ]
            .map(RtmpSessionRoleDto::from)
        ),
        ["client", "publisher", "subscriber"]
    );
    assert_eq!(
        names(
            [
                RtmpSessionControlAction::Client,
                RtmpSessionControlAction::Publisher,
                RtmpSessionControlAction::Subscriber,
            ]
            .map(RtmpSessionControlActionDto::from)
        ),
        ["client", "publisher", "subscriber"]
    );
    assert_eq!(
        names([false, true].map(RtmpSessionControlOutcomeDto::from)),
        ["requested", "already_requested"]
    );
}
