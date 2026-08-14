use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    HealthFailure,
    operational_event::{EventName, EventOutcome, EventPage, OperationalEvent},
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventPageV1Response {
    events: Vec<OperationalEventDto<V1EventNameDto>>,
    cursor: u64,
    has_more: bool,
    oldest_cursor: Option<u64>,
}

impl From<EventPage> for EventPageV1Response {
    fn from(page: EventPage) -> Self {
        Self {
            events: page.events.iter().map(OperationalEventDto::v1).collect(),
            cursor: page.cursor,
            has_more: page.has_more,
            oldest_cursor: page.oldest_cursor,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventPageV2Response {
    events: Vec<OperationalEventDto<V2EventNameDto>>,
    cursor: u64,
    latest_cursor: u64,
    has_more: bool,
    oldest_cursor: Option<u64>,
}

impl From<EventPage> for EventPageV2Response {
    fn from(page: EventPage) -> Self {
        Self {
            events: page.events.iter().map(OperationalEventDto::v2).collect(),
            cursor: page.cursor,
            latest_cursor: page.latest_cursor,
            has_more: page.has_more,
            oldest_cursor: page.oldest_cursor,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationalEventDto<N> {
    cursor: u64,
    timestamp_unix_ms: Option<u64>,
    event: N,
    outcome: EventOutcomeDto,
    revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<String>,
}

impl OperationalEventDto<V1EventNameDto> {
    pub(crate) fn v1(event: &OperationalEvent) -> Self {
        Self::new(event, event.event.into())
    }
}

impl OperationalEventDto<V2EventNameDto> {
    pub(crate) fn v2(event: &OperationalEvent) -> Self {
        Self::new(event, event.event.into())
    }
}

impl<N> OperationalEventDto<N> {
    fn new(event: &OperationalEvent, name: N) -> Self {
        Self {
            cursor: event.cursor,
            timestamp_unix_ms: event.timestamp_unix_ms,
            event: name,
            outcome: (&event.outcome).into(),
            revision: event
                .revision
                .as_ref()
                .map(|revision| revision.as_str().to_owned()),
            certificate: event.certificate.clone(),
            correlation_id: event.correlation_id.clone(),
            actor: event.actor.clone(),
            source: event.source.clone(),
            operation: event.operation.clone(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum V1EventNameDto {
    GenerationPrepare,
    GenerationActivate,
    GenerationRollback,
    GenerationDrain,
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
    UpstreamEndpointEjection,
    UpstreamEndpointRecovery,
    Unknown,
}

impl From<EventName> for V1EventNameDto {
    fn from(event: EventName) -> Self {
        match event {
            EventName::GenerationPrepare => Self::GenerationPrepare,
            EventName::GenerationActivate => Self::GenerationActivate,
            EventName::GenerationRollback => Self::GenerationRollback,
            EventName::GenerationDrain => Self::GenerationDrain,
            EventName::GenerationStart => Self::GenerationStart,
            EventName::ConfigurationReload => Self::ConfigurationReload,
            EventName::ImportCompleted => Self::ImportCompleted,
            EventName::ControlOperation => Self::ControlOperation,
            EventName::ProcessShutdown => Self::ProcessShutdown,
            EventName::ListenerAdministrativeState => Self::ListenerAdministrativeState,
            EventName::PoolAdministrativeState => Self::PoolAdministrativeState,
            EventName::ServerUpdate => Self::ServerUpdate,
            EventName::RtmpConnect => Self::RtmpConnect,
            EventName::RtmpPublish => Self::RtmpPublish,
            EventName::RtmpPlay => Self::RtmpPlay,
            EventName::RtmpDisconnect => Self::RtmpDisconnect,
            EventName::RtmpAccess => Self::RtmpAccess,
            EventName::CertificateRenewal => Self::CertificateRenewal,
            EventName::CertificateActivation => Self::CertificateActivation,
            EventName::UpstreamEndpointEjection => Self::UpstreamEndpointEjection,
            EventName::UpstreamEndpointRecovery => Self::UpstreamEndpointRecovery,
            EventName::CertificateRevocation
            | EventName::CertificateDeletion
            | EventName::CertificateAccountRollover
            | EventName::CertificateJobControl
            | EventName::Unknown => Self::Unknown,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum V2EventNameDto {
    GenerationPrepare,
    GenerationActivate,
    GenerationRollback,
    GenerationDrain,
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
    CertificateActivated,
    CertificateRevocation,
    CertificateDeletion,
    CertificateAccountRollover,
    CertificateJobControl,
    UpstreamEndpointEjection,
    UpstreamEndpointRecovery,
    Unknown,
}

impl From<EventName> for V2EventNameDto {
    fn from(event: EventName) -> Self {
        match event {
            EventName::GenerationPrepare => Self::GenerationPrepare,
            EventName::GenerationActivate => Self::GenerationActivate,
            EventName::GenerationRollback => Self::GenerationRollback,
            EventName::GenerationDrain => Self::GenerationDrain,
            EventName::GenerationStart => Self::GenerationStart,
            EventName::ConfigurationReload => Self::ConfigurationReload,
            EventName::ImportCompleted => Self::ImportCompleted,
            EventName::ControlOperation => Self::ControlOperation,
            EventName::ProcessShutdown => Self::ProcessShutdown,
            EventName::ListenerAdministrativeState => Self::ListenerAdministrativeState,
            EventName::PoolAdministrativeState => Self::PoolAdministrativeState,
            EventName::ServerUpdate => Self::ServerUpdate,
            EventName::RtmpConnect => Self::RtmpConnect,
            EventName::RtmpPublish => Self::RtmpPublish,
            EventName::RtmpPlay => Self::RtmpPlay,
            EventName::RtmpDisconnect => Self::RtmpDisconnect,
            EventName::RtmpAccess => Self::RtmpAccess,
            EventName::CertificateRenewal => Self::CertificateRenewal,
            EventName::CertificateActivation => Self::CertificateActivated,
            EventName::CertificateRevocation => Self::CertificateRevocation,
            EventName::CertificateDeletion => Self::CertificateDeletion,
            EventName::CertificateAccountRollover => Self::CertificateAccountRollover,
            EventName::CertificateJobControl => Self::CertificateJobControl,
            EventName::UpstreamEndpointEjection => Self::UpstreamEndpointEjection,
            EventName::UpstreamEndpointRecovery => Self::UpstreamEndpointRecovery,
            EventName::Unknown => Self::Unknown,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(untagged)]
enum EventOutcomeDto {
    Simple(SimpleEventOutcomeDto),
    Ejected(EjectedOutcomeDto),
    Recovered(RecoveredOutcomeDto),
}

impl From<&EventOutcome> for EventOutcomeDto {
    fn from(outcome: &EventOutcome) -> Self {
        match outcome {
            EventOutcome::Prepared => Self::Simple(SimpleEventOutcomeDto::Prepared),
            EventOutcome::Rejected => Self::Simple(SimpleEventOutcomeDto::Rejected),
            EventOutcome::Activated => Self::Simple(SimpleEventOutcomeDto::Activated),
            EventOutcome::Quarantined => Self::Simple(SimpleEventOutcomeDto::Quarantined),
            EventOutcome::Requested => Self::Simple(SimpleEventOutcomeDto::Requested),
            EventOutcome::Applied => Self::Simple(SimpleEventOutcomeDto::Applied),
            EventOutcome::Failed => Self::Simple(SimpleEventOutcomeDto::Failed),
            EventOutcome::Unknown => Self::Simple(SimpleEventOutcomeDto::Unknown),
            EventOutcome::Ejected {
                pool,
                server,
                reason,
                failure_count,
                ejection_count,
                ejected_at_unix_ms,
                ejection_until_unix_ms,
            } => Self::Ejected(EjectedOutcomeDto {
                r#type: "ejected",
                pool: pool.clone(),
                server: server.clone(),
                reason: (*reason).into(),
                failure_count: *failure_count,
                ejection_count: *ejection_count,
                ejected_at_unix_ms: *ejected_at_unix_ms,
                ejection_until_unix_ms: *ejection_until_unix_ms,
            }),
            EventOutcome::Recovered {
                pool,
                server,
                reason,
                recovery_count,
                recovered_at_unix_ms,
            } => Self::Recovered(RecoveredOutcomeDto {
                r#type: "recovered",
                pool: pool.clone(),
                server: server.clone(),
                reason: reason.map(Into::into),
                recovery_count: *recovery_count,
                recovered_at_unix_ms: *recovered_at_unix_ms,
            }),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum SimpleEventOutcomeDto {
    Prepared,
    Rejected,
    Activated,
    Quarantined,
    Requested,
    Applied,
    Failed,
    Unknown,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct EjectedOutcomeDto {
    r#type: &'static str,
    pool: String,
    server: String,
    reason: HealthFailureDto,
    failure_count: u64,
    ejection_count: u64,
    ejected_at_unix_ms: u64,
    ejection_until_unix_ms: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveredOutcomeDto {
    r#type: &'static str,
    pool: String,
    server: String,
    reason: Option<HealthFailureDto>,
    recovery_count: u64,
    recovered_at_unix_ms: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum HealthFailureDto {
    Timeout,
    ConnectFailed,
    UnexpectedStatus,
    ProtocolError,
}

impl From<HealthFailure> for HealthFailureDto {
    fn from(reason: HealthFailure) -> Self {
        match reason {
            HealthFailure::Timeout => Self::Timeout,
            HealthFailure::ConnectFailed => Self::ConnectFailed,
            HealthFailure::UnexpectedStatus => Self::UnexpectedStatus,
            HealthFailure::ProtocolError => Self::ProtocolError,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct SseReadyDto {
    cursor: u64,
}

impl SseReadyDto {
    pub(crate) const fn new(cursor: u64) -> Self {
        Self { cursor }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SseResyncDto {
    cursor: u64,
    oldest_cursor: Option<u64>,
    latest_cursor: u64,
}

impl SseResyncDto {
    pub(crate) const fn new(cursor: u64, oldest_cursor: Option<u64>, latest_cursor: u64) -> Self {
        Self {
            cursor,
            oldest_cursor,
            latest_cursor,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct SseShutdownDto {
    reason: &'static str,
}

impl SseShutdownDto {
    pub(crate) const fn server_shutdown() -> Self {
        Self {
            reason: "server_shutdown",
        }
    }
}

#[cfg(test)]
mod tests {
    use schemars::generate::SchemaSettings;
    use serde_json::json;

    use super::*;

    #[test]
    fn sse_control_dtos_preserve_exact_payloads() {
        assert_eq!(
            serde_json::to_value(SseReadyDto::new(7)).expect("ready"),
            json!({ "cursor": 7 })
        );
        assert_eq!(
            serde_json::to_value(SseResyncDto::new(7, None, 9)).expect("resync"),
            json!({ "cursor": 7, "oldestCursor": null, "latestCursor": 9 })
        );
        assert_eq!(
            serde_json::to_value(SseShutdownDto::server_shutdown()).expect("shutdown"),
            json!({ "reason": "server_shutdown" })
        );
    }

    #[test]
    fn event_schema_requires_v2_cursor_and_preserves_nullable_revision() {
        let generator = SchemaSettings::default().for_serialize().into_generator();
        let schema = serde_json::to_value(generator.into_root_schema_for::<EventPageV2Response>())
            .expect("event schema");
        let names =
            serde_json::to_value(schemars::schema_for!(V2EventNameDto)).expect("event name schema");

        assert!(
            schema["required"]
                .as_array()
                .expect("required")
                .contains(&json!("latestCursor"))
        );
        assert!(
            serde_json::to_string(&schema)
                .expect("schema JSON")
                .contains("revision")
        );
        assert!(
            names["enum"]
                .as_array()
                .expect("event names")
                .contains(&json!("certificate_activated"))
        );
        assert!(
            names["enum"]
                .as_array()
                .expect("event names")
                .contains(&json!("certificate_revocation"))
        );
    }
}
