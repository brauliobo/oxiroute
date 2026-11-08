#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationalEvent {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: EventName,
    pub outcome: EventOutcome,
    pub revision: Option<EffectiveRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum EventName {
    GenerationPrepare,
    GenerationActivate,
    GenerationRollback,
    GenerationDrain,
    GenerationRetire,
    GenerationStart,
    ConfigurationReload,
    ImportCompleted,
    ControlOperation,
    ProcessShutdown,
    ListenerAdministrativeState,
    PoolAdministrativeState,
    ServerUpdate,
    RtmpConnect,
    RtmpPublish,
    RtmpPlay,
    RtmpDisconnect,
    RtmpAccess,
    CertificateRenewal,
    CertificateActivation,
    CertificateRevocation,
    CertificateDeletion,
    CertificateAccountRollover,
    CertificateJobControl,
    UpstreamEndpointEjection,
    UpstreamEndpointRecovery,
    UpstreamEndpointHealth,
    ServerEvacuation,
    Unknown,
}

impl Serialize for EventName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(v1_serialized_event_name(*self))
    }
}

impl EventName {
    pub(super) fn parse(value: &str) -> Self {
        match value {
            "generation_prepare" => Self::GenerationPrepare,
            "generation_activate" => Self::GenerationActivate,
            "generation_rollback" => Self::GenerationRollback,
            "generation_drain" => Self::GenerationDrain,
            "generation_retire" => Self::GenerationRetire,
            "generation_start" => Self::GenerationStart,
            "configuration_reload" => Self::ConfigurationReload,
            "import_completed" => Self::ImportCompleted,
            "control_operation" => Self::ControlOperation,
            "process_shutdown" => Self::ProcessShutdown,
            "listener_administrative_state" => Self::ListenerAdministrativeState,
            "pool_administrative_state" => Self::PoolAdministrativeState,
            "server_update" => Self::ServerUpdate,
            "rtmp_connect" => Self::RtmpConnect,
            "rtmp_publish" => Self::RtmpPublish,
            "rtmp_play" => Self::RtmpPlay,
            "rtmp_disconnect" => Self::RtmpDisconnect,
            "rtmp_access" => Self::RtmpAccess,
            "certificate_renewal" => Self::CertificateRenewal,
            "certificate_activated" => Self::CertificateActivation,
            "certificate_revocation" => Self::CertificateRevocation,
            "certificate_deletion" => Self::CertificateDeletion,
            "certificate_account_rollover" => Self::CertificateAccountRollover,
            "certificate_job_control" => Self::CertificateJobControl,
            "upstream_endpoint_ejection" => Self::UpstreamEndpointEjection,
            "upstream_endpoint_recovery" => Self::UpstreamEndpointRecovery,
            "upstream_endpoint_health" => Self::UpstreamEndpointHealth,
            "server_evacuation" => Self::ServerEvacuation,
            _ => Self::Unknown,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GenerationPrepare => "generation_prepare",
            Self::GenerationActivate => "generation_activate",
            Self::GenerationRollback => "generation_rollback",
            Self::GenerationDrain => "generation_drain",
            Self::GenerationRetire => "generation_retire",
            Self::GenerationStart => "generation_start",
            Self::ConfigurationReload => "configuration_reload",
            Self::ImportCompleted => "import_completed",
            Self::ControlOperation => "control_operation",
            Self::ProcessShutdown => "process_shutdown",
            Self::ListenerAdministrativeState => "listener_administrative_state",
            Self::PoolAdministrativeState => "pool_administrative_state",
            Self::ServerUpdate => "server_update",
            Self::RtmpConnect => "rtmp_connect",
            Self::RtmpPublish => "rtmp_publish",
            Self::RtmpPlay => "rtmp_play",
            Self::RtmpDisconnect => "rtmp_disconnect",
            Self::RtmpAccess => "rtmp_access",
            Self::CertificateRenewal => "certificate_renewal",
            Self::CertificateActivation => "certificate_activated",
            Self::CertificateRevocation => "certificate_revocation",
            Self::CertificateDeletion => "certificate_deletion",
            Self::CertificateAccountRollover => "certificate_account_rollover",
            Self::CertificateJobControl => "certificate_job_control",
            Self::UpstreamEndpointEjection => "upstream_endpoint_ejection",
            Self::UpstreamEndpointRecovery => "upstream_endpoint_recovery",
            Self::UpstreamEndpointHealth => "upstream_endpoint_health",
            Self::ServerEvacuation => "server_evacuation",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) const fn v1_serialized_event_name(event: EventName) -> &'static str {
    match event {
        EventName::CertificateActivation => "certificate_activation",
        EventName::CertificateRevocation
        | EventName::CertificateDeletion
        | EventName::CertificateAccountRollover
        | EventName::CertificateJobControl
        | EventName::GenerationRetire
        | EventName::UpstreamEndpointHealth
        | EventName::ServerEvacuation => "unknown",
        _ => event.as_str(),
    }
}

pub(crate) const fn v1_sse_event_name(event: EventName) -> &'static str {
    match event {
        EventName::CertificateRevocation
        | EventName::CertificateDeletion
        | EventName::CertificateAccountRollover
        | EventName::CertificateJobControl
        | EventName::GenerationRetire
        | EventName::ServerEvacuation => "unknown",
        _ => event.as_str(),
    }
}

pub(crate) fn v1_event_value(event: &OperationalEvent) -> serde_json::Value {
    serde_json::to_value(crate::rtmp::api::dto::OperationalEventDto::v1(event))
        .expect("v1 operational event DTO serializes")
}

pub(crate) fn v2_event_value(event: &OperationalEvent) -> serde_json::Value {
    serde_json::to_value(crate::rtmp::api::dto::OperationalEventDto::v2(event))
        .expect("v2 operational event DTO serializes")
}

#[derive(Clone, Debug)]
pub(crate) enum EventOutcome {
    Prepared,
    Rejected,
    Activated,
    Quarantined,
    Requested,
    Applied,
    Failed,
    Retiring,
    Drained,
    ForcedClose,
    CapReached,
    Ejected {
        pool: String,
        server: String,
        reason: HealthFailure,
        failure_count: u64,
        ejection_count: u64,
        ejected_at_unix_ms: u64,
        ejection_until_unix_ms: u64,
    },
    Recovered {
        pool: String,
        server: String,
        reason: Option<HealthFailure>,
        recovery_count: u64,
        recovered_at_unix_ms: u64,
    },
    HealthChanged {
        pool: String,
        server: String,
        previous: EndpointHealthState,
        next: EndpointHealthState,
        reason: Option<HealthFailure>,
        changed_at_unix_ms: u64,
    },
    Evacuated {
        pool: String,
        server: Option<String>,
        force: bool,
        report: EvacuationReport,
    },
    Unknown,
}

impl Serialize for EventOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Prepared => serializer.serialize_str("prepared"),
            Self::Rejected => serializer.serialize_str("rejected"),
            Self::Activated => serializer.serialize_str("activated"),
            Self::Quarantined => serializer.serialize_str("quarantined"),
            Self::Requested => serializer.serialize_str("requested"),
            Self::Applied => serializer.serialize_str("applied"),
            Self::Failed => serializer.serialize_str("failed"),
            Self::Retiring => serializer.serialize_str("retiring"),
            Self::Drained => serializer.serialize_str("drained"),
            Self::ForcedClose => serializer.serialize_str("forced_close"),
            Self::CapReached => serializer.serialize_str("cap_reached"),
            Self::Unknown => serializer.serialize_str("unknown"),
            Self::Ejected {
                pool,
                server,
                reason,
                failure_count,
                ejection_count,
                ejected_at_unix_ms,
                ejection_until_unix_ms,
            } => {
                let mut value = serializer.serialize_struct("EventOutcome", 8)?;
                value.serialize_field("type", "ejected")?;
                value.serialize_field("pool", pool)?;
                value.serialize_field("server", server)?;
                value.serialize_field("reason", reason)?;
                value.serialize_field("failureCount", failure_count)?;
                value.serialize_field("ejectionCount", ejection_count)?;
                value.serialize_field("ejectedAtUnixMs", ejected_at_unix_ms)?;
                value.serialize_field("ejectionUntilUnixMs", ejection_until_unix_ms)?;
                value.end()
            }
            Self::Recovered {
                pool,
                server,
                reason,
                recovery_count,
                recovered_at_unix_ms,
            } => {
                let mut value = serializer.serialize_struct("EventOutcome", 6)?;
                value.serialize_field("type", "recovered")?;
                value.serialize_field("pool", pool)?;
                value.serialize_field("server", server)?;
                value.serialize_field("reason", reason)?;
                value.serialize_field("recoveryCount", recovery_count)?;
                value.serialize_field("recoveredAtUnixMs", recovered_at_unix_ms)?;
                value.end()
            }
            Self::HealthChanged {
                pool,
                server,
                previous,
                next,
                reason,
                changed_at_unix_ms,
            } => {
                let mut value = serializer.serialize_struct("EventOutcome", 7)?;
                value.serialize_field("type", "health_changed")?;
                value.serialize_field("pool", pool)?;
                value.serialize_field("server", server)?;
                value.serialize_field("previous", previous)?;
                value.serialize_field("next", next)?;
                value.serialize_field("reason", reason)?;
                value.serialize_field("changedAtUnixMs", changed_at_unix_ms)?;
                value.end()
            }
            Self::Evacuated {
                pool,
                server,
                force,
                report,
            } => {
                let mut value = serializer.serialize_struct("EventOutcome", 7)?;
                value.serialize_field("type", "evacuated")?;
                value.serialize_field("pool", pool)?;
                if let Some(server) = server {
                    value.serialize_field("server", server)?;
                }
                value.serialize_field("force", force)?;
                value.serialize_field("cancelledAndRedispatched", &report.redispatched)?;
                value.serialize_field("forced", &report.forced)?;
                value.serialize_field("leftRunning", &report.left_running)?;
                value.end()
            }
        }
    }
}

impl EventOutcome {
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Rejected => "rejected",
            Self::Activated => "activated",
            Self::Quarantined => "quarantined",
            Self::Requested => "requested",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Retiring => "retiring",
            Self::Drained => "drained",
            Self::ForcedClose => "forced_close",
            Self::CapReached => "cap_reached",
            Self::Ejected { .. } => "ejected",
            Self::Recovered { .. } => "recovered",
            Self::HealthChanged { .. } => "health_changed",
            Self::Evacuated { .. } => "evacuated",
            Self::Unknown => "unknown",
        }
    }

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "prepared" => Self::Prepared,
            "rejected" => Self::Rejected,
            "activated" => Self::Activated,
            "quarantined" => Self::Quarantined,
            "requested" => Self::Requested,
            "applied" => Self::Applied,
            "failed" => Self::Failed,
            "retiring" => Self::Retiring,
            "drained" => Self::Drained,
            "forced_close" => Self::ForcedClose,
            "cap_reached" => Self::CapReached,
            _ => Self::Unknown,
        }
    }
}

pub(crate) struct EventPage {
    pub events: Vec<OperationalEvent>,
    pub cursor: u64,
    pub has_more: bool,
    pub oldest_cursor: Option<u64>,
    pub latest_cursor: u64,
    pub cursor_lost: bool,
}

impl OperationalEvent {
    pub(super) fn new(
        cursor: u64,
        timestamp_unix_ms: Option<u64>,
        event: &str,
        outcome: &str,
        revision: Option<&EffectiveRevision>,
    ) -> Self {
        Self {
            cursor,
            timestamp_unix_ms,
            event: EventName::parse(event),
            outcome: EventOutcome::parse(outcome),
            revision: revision.cloned(),
            certificate: None,
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        }
    }
}
