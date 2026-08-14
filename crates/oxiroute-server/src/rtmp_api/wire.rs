use schemars::JsonSchema;
use serde::Serialize;

macro_rules! wire_enum {
    ($name:ident from $domain:ty { $($source:path => $target:ident),+ $(,)? }) => {
        #[derive(JsonSchema, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name {
            $($target),+
        }

        impl From<$domain> for $name {
            fn from(value: $domain) -> Self {
                match value {
                    $($source => Self::$target),+
                }
            }
        }
    };
}

wire_enum!(RelayPhaseDto from oxiroute_rtmp::RtmpRelayPhase {
    oxiroute_rtmp::RtmpRelayPhase::Connecting => Connecting,
    oxiroute_rtmp::RtmpRelayPhase::Publishing => Publishing,
    oxiroute_rtmp::RtmpRelayPhase::Pulling => Pulling,
    oxiroute_rtmp::RtmpRelayPhase::Backoff => Backoff,
    oxiroute_rtmp::RtmpRelayPhase::Stopped => Stopped,
});
wire_enum!(RelayFailureDto from oxiroute_rtmp::RtmpRelayFailure {
    oxiroute_rtmp::RtmpRelayFailure::Policy => Policy,
    oxiroute_rtmp::RtmpRelayFailure::Connect => Connect,
    oxiroute_rtmp::RtmpRelayFailure::Handshake => Handshake,
    oxiroute_rtmp::RtmpRelayFailure::Session => Session,
    oxiroute_rtmp::RtmpRelayFailure::Transport => Transport,
    oxiroute_rtmp::RtmpRelayFailure::Source => Source,
    oxiroute_rtmp::RtmpRelayFailure::Thread => Thread,
});
wire_enum!(RelayDnsRefreshFailureDto from oxiroute_rtmp::RtmpDnsRefreshFailure {
    oxiroute_rtmp::RtmpDnsRefreshFailure::Resolution => Resolution,
    oxiroute_rtmp::RtmpDnsRefreshFailure::AddressSet => AddressSet,
    oxiroute_rtmp::RtmpDnsRefreshFailure::Policy => Policy,
    oxiroute_rtmp::RtmpDnsRefreshFailure::DirectLoop => DirectLoop,
    oxiroute_rtmp::RtmpDnsRefreshFailure::FamilyMismatch => FamilyMismatch,
});
wire_enum!(RecorderNotificationDto from oxiroute_rtmp::RecorderNotification {
    oxiroute_rtmp::RecorderNotification::Started => Started,
    oxiroute_rtmp::RecorderNotification::Stopped => Stopped,
    oxiroute_rtmp::RecorderNotification::Failed => Failed,
});
wire_enum!(RecorderErrorCodeDto from oxiroute_rtmp::RecorderErrorCode {
    oxiroute_rtmp::RecorderErrorCode::OpenFailed => OpenFailed,
    oxiroute_rtmp::RecorderErrorCode::WriteFailed => WriteFailed,
    oxiroute_rtmp::RecorderErrorCode::CloseFailed => CloseFailed,
    oxiroute_rtmp::RecorderErrorCode::BackendUnavailable => BackendUnavailable,
    oxiroute_rtmp::RecorderErrorCode::FileSyncFailed => FileSyncFailed,
    oxiroute_rtmp::RecorderErrorCode::PublishFailed => PublishFailed,
    oxiroute_rtmp::RecorderErrorCode::DirectorySyncFailed => DirectorySyncFailed,
    oxiroute_rtmp::RecorderErrorCode::QueueDiscontinuity => QueueDiscontinuity,
    oxiroute_rtmp::RecorderErrorCode::UnsupportedCodec => UnsupportedCodec,
    oxiroute_rtmp::RecorderErrorCode::ShutdownTimedOut => ShutdownTimedOut,
    oxiroute_rtmp::RecorderErrorCode::WorkerPanicked => WorkerPanicked,
    oxiroute_rtmp::RecorderErrorCode::StalePublisher => StalePublisher,
});
wire_enum!(RtmpSessionRoleDto from oxiroute_rtmp::RtmpSessionRole {
    oxiroute_rtmp::RtmpSessionRole::Client => Client,
    oxiroute_rtmp::RtmpSessionRole::Publisher => Publisher,
    oxiroute_rtmp::RtmpSessionRole::Subscriber => Subscriber,
});
wire_enum!(RtmpSessionControlActionDto from oxiroute_rtmp::RtmpSessionControlAction {
    oxiroute_rtmp::RtmpSessionControlAction::Client => Client,
    oxiroute_rtmp::RtmpSessionControlAction::Publisher => Publisher,
    oxiroute_rtmp::RtmpSessionControlAction::Subscriber => Subscriber,
});

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RtmpSessionControlOutcomeDto {
    Requested,
    AlreadyRequested,
}

impl From<bool> for RtmpSessionControlOutcomeDto {
    fn from(already_requested: bool) -> Self {
        if already_requested {
            Self::AlreadyRequested
        } else {
            Self::Requested
        }
    }
}

#[cfg(test)]
mod tests {
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
}
