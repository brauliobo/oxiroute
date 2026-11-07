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
mod tests;
