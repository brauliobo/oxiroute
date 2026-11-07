#[allow(clippy::wildcard_imports)]
use super::*;

macro_rules! projected_enum {
    ($name:ident from $domain:ty { $($source:path => $target:ident),+ $(,)? }) => {
        #[derive(JsonSchema, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(super) enum $name {
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

projected_enum!(AdministrativeStateDto from AdministrativeState {
    AdministrativeState::Ready => Ready,
    AdministrativeState::Drain => Drain,
    AdministrativeState::Maintenance => Maintenance,
});
projected_enum!(ComponentStateDto from ComponentState {
    ComponentState::Healthy => Healthy,
    ComponentState::Degraded => Degraded,
    ComponentState::Unsupported => Unsupported,
});
projected_enum!(AuditComponentStateDto from AuditComponentState {
    AuditComponentState::Healthy => Healthy,
    AuditComponentState::Degraded => Degraded,
    AuditComponentState::Memory => Memory,
});
projected_enum!(CertbotWatcherHealthDto from CertbotWatcherHealth {
    CertbotWatcherHealth::Healthy => Healthy,
    CertbotWatcherHealth::Degraded => Degraded,
    CertbotWatcherHealth::Stopped => Stopped,
});
projected_enum!(ObservedTransportDto from ObservedTransport {
    ObservedTransport::Http => Http,
    ObservedTransport::Rtmp => Rtmp,
    ObservedTransport::Forward => Forward,
    ObservedTransport::Cache => Cache,
    ObservedTransport::Tcp => Tcp,
    ObservedTransport::Udp => Udp,
    ObservedTransport::H3 => H3,
    ObservedTransport::Acme => Acme,
});
projected_enum!(TransportOutcomeDto from TransportOutcome {
    TransportOutcome::Success => Success,
    TransportOutcome::ClientError => ClientError,
    TransportOutcome::ServerError => ServerError,
    TransportOutcome::UpstreamError => UpstreamError,
    TransportOutcome::Timeout => Timeout,
    TransportOutcome::Rejected => Rejected,
    TransportOutcome::Cancelled => Cancelled,
    TransportOutcome::InternalError => InternalError,
    TransportOutcome::Degraded => Degraded,
});
projected_enum!(RelayPhaseDto from oxiroute_rtmp::RtmpRelayPhase {
    oxiroute_rtmp::RtmpRelayPhase::Connecting => Connecting,
    oxiroute_rtmp::RtmpRelayPhase::Publishing => Publishing,
    oxiroute_rtmp::RtmpRelayPhase::Pulling => Pulling,
    oxiroute_rtmp::RtmpRelayPhase::Backoff => Backoff,
    oxiroute_rtmp::RtmpRelayPhase::Stopped => Stopped,
});
projected_enum!(RelayFailureDto from oxiroute_rtmp::RtmpRelayFailure {
    oxiroute_rtmp::RtmpRelayFailure::Policy => Policy,
    oxiroute_rtmp::RtmpRelayFailure::Connect => Connect,
    oxiroute_rtmp::RtmpRelayFailure::Handshake => Handshake,
    oxiroute_rtmp::RtmpRelayFailure::Session => Session,
    oxiroute_rtmp::RtmpRelayFailure::Transport => Transport,
    oxiroute_rtmp::RtmpRelayFailure::Source => Source,
    oxiroute_rtmp::RtmpRelayFailure::Thread => Thread,
});
projected_enum!(RelayDnsRefreshFailureDto from oxiroute_rtmp::RtmpDnsRefreshFailure {
    oxiroute_rtmp::RtmpDnsRefreshFailure::Resolution => Resolution,
    oxiroute_rtmp::RtmpDnsRefreshFailure::AddressSet => AddressSet,
    oxiroute_rtmp::RtmpDnsRefreshFailure::Policy => Policy,
    oxiroute_rtmp::RtmpDnsRefreshFailure::DirectLoop => DirectLoop,
    oxiroute_rtmp::RtmpDnsRefreshFailure::FamilyMismatch => FamilyMismatch,
});

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RecorderPhaseDto {
    Idle,
    Starting,
    Recording,
    Stopping,
    Failed,
}

impl From<RecorderPhase> for RecorderPhaseDto {
    fn from(phase: RecorderPhase) -> Self {
        match phase {
            RecorderPhase::Idle => Self::Idle,
            RecorderPhase::Starting { .. } => Self::Starting,
            RecorderPhase::Recording { .. } => Self::Recording,
            RecorderPhase::Stopping { .. } => Self::Stopping,
            RecorderPhase::Failed { .. } => Self::Failed,
        }
    }
}
