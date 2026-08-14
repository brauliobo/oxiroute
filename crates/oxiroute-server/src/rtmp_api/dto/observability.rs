use oxiroute_rtmp::{RecorderPhase, RtmpCatalogSnapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::management::{ListenerDto, PoolDto};
use super::{DecimalCounter, management::LatencyDto};
use crate::operational_event::{AuditComponentState, AuditStatus};
use crate::{
    AccessRecord, AcmeManagedCertificateSnapshot, AdministrativeState, CertbotCertificateSnapshot,
    CertbotWatcherHealth, CertbotWatcherSnapshot, ComponentState, ComponentStatus,
    DirectFileCertificateSnapshot, DirectFileWatcherSnapshot, GenerationStatus, HostSnapshot,
    ListenerRuntimeState, ListenerSnapshot, ObservedTransport, ProcessSnapshot, RuntimeMode,
    RuntimeSnapshot, TopologyNodeKind, TopologySnapshot, TrafficSnapshot,
    TransportOperationCountSnapshot, TransportOperationSnapshot, TransportOutcome,
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusResponse {
    schema_version: u8,
    build_version: &'static str,
    disk_revision: Option<String>,
    candidate_revision: Option<String>,
    active_revision: Option<String>,
    previous_revision: Option<String>,
    degraded: bool,
    active_generation_age_ms: u64,
    components: StatusComponentsDto,
    certificates: StatusCertificatesDto,
    audit: AuditStatusDto,
    capabilities: CapabilitiesResponse,
    listeners: Vec<ListenerDto>,
    tls_profiles: Vec<TlsProfileDto>,
}

impl StatusResponse {
    pub(crate) fn project(
        runtime: RuntimeSnapshot,
        generation: Option<&GenerationStatus>,
        audit: AuditStatus,
        topology: &TopologySnapshot,
        mode: RuntimeMode,
    ) -> Result<Self, String> {
        let capabilities = CapabilitiesResponse::project(&runtime.listeners, mode)
            .map_err(|error| error.to_string())?;
        let listeners = runtime.listeners.into_iter().map(Into::into).collect();
        Ok(Self {
            schema_version: 1,
            build_version: crate::cli::BUILD_VERSION,
            disk_revision: generation
                .and_then(|status| status.disk_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            candidate_revision: generation
                .and_then(|status| status.candidate_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            active_revision: generation
                .and_then(|status| status.active_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            previous_revision: generation
                .and_then(|status| status.previous_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            degraded: generation.is_none_or(|status| status.degraded),
            active_generation_age_ms: runtime.generation_age_ms,
            components: StatusComponentsDto {
                process: runtime.process.status.into(),
                host: runtime.host.status.into(),
                generation: generation_component(generation),
                audit: audit.clone().into(),
            },
            certificates: StatusCertificatesDto {
                certbot: runtime
                    .certbot_certificates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                acme_managed: runtime
                    .acme_managed_certificates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                direct_files: runtime
                    .direct_file_certificates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
            audit: audit.into(),
            capabilities,
            listeners,
            tls_profiles: topology
                .nodes()
                .iter()
                .filter(|node| node.kind == TopologyNodeKind::TlsProfile)
                .map(tls_profile)
                .collect::<Result<_, _>>()?,
        })
    }
}

fn tls_profile(node: &crate::TopologyNode) -> Result<TlsProfileDto, String> {
    let client_auth = node
        .attributes
        .get("clientAuth")
        .ok_or_else(|| "TLS profile is missing clientAuth".to_owned())?;
    Ok(TlsProfileDto {
        name: node.name.clone(),
        client_auth: TlsClientAuthDto {
            mode: client_auth
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "TLS clientAuth is missing mode".to_owned())?
                .to_owned(),
            ca_configured: client_auth
                .get("caConfigured")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| "TLS clientAuth is missing caConfigured".to_owned())?,
            allowed_dns_name_count: client_auth
                .get("allowedDnsNameCount")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "TLS clientAuth is missing allowedDnsNameCount".to_owned())?,
        },
    })
}

#[derive(JsonSchema, Serialize)]
struct StatusComponentsDto {
    process: ComponentStatusDto,
    host: ComponentStatusDto,
    generation: ComponentStatusDto,
    audit: AuditStatusDto,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusCertificatesDto {
    certbot: Vec<CertbotCertificateDto>,
    acme_managed: Vec<AcmeManagedCertificateDto>,
    direct_files: Vec<DirectFileCertificateDto>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsProfileDto {
    name: String,
    client_auth: TlsClientAuthDto,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsClientAuthDto {
    mode: String,
    ca_configured: bool,
    allowed_dns_name_count: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadinessResponse {
    ready: bool,
    build_version: &'static str,
    active_revision: Option<String>,
}

impl ReadinessResponse {
    pub(crate) fn project(runtime: &RuntimeSnapshot, generation: Option<GenerationStatus>) -> Self {
        let ready = generation.as_ref().is_some_and(|status| {
            status.active_revision.is_some()
                && !status.degraded
                && runtime.process.administrative_state == AdministrativeState::Ready
                && runtime.listeners.iter().all(|listener| {
                    listener.state == ListenerRuntimeState::Listening
                        && listener.administrative_state == AdministrativeState::Ready
                })
        });
        Self {
            ready,
            build_version: crate::cli::BUILD_VERSION,
            active_revision: generation
                .and_then(|status| status.active_revision)
                .map(|revision| revision.as_str().to_owned()),
        }
    }

    pub(crate) const fn ready(&self) -> bool {
        self.ready
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CapabilitiesResponse {
    schema_version: u8,
    supervision: SupervisionCapabilityDto,
    udp: UdpCapabilityDto,
    http3: Http3CapabilitiesDto,
}

impl CapabilitiesResponse {
    pub(crate) fn project(
        listeners: &[ListenerSnapshot],
        mode: RuntimeMode,
    ) -> Result<Self, serde_json::Error> {
        serde_json::from_value(crate::http3::capability_snapshot(listeners, mode))
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct SupervisionCapabilityDto {
    mode: RuntimeModeDto,
    #[serde(rename = "descriptorAdoption")]
    descriptor_adoption: DescriptorAdoptionDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DescriptorAdoptionDto {
    status: DescriptorAdoptionStatusDto,
    manifest_version: u8,
    datagram: bool,
    quic: bool,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct Http3CapabilitiesDto {
    reverse: H3CapabilityDto,
    forward: H3CapabilityDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UdpCapabilityDto {
    status: CapabilityStatusDto,
    supported: bool,
    listeners: Vec<String>,
    configured_listeners: Vec<String>,
    transport: UdpTransportDto,
    drain: GracefulDto,
    fallback: NoneDto,
    blocked_reason: Option<BlockedReasonDto>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct H3CapabilityDto {
    status: CapabilityStatusDto,
    supported: bool,
    listeners: Vec<String>,
    configured_listeners: Vec<String>,
    transport: QuicTransportDto,
    alpn: Vec<String>,
    tls_min_version: String,
    zero_rtt: DisabledDto,
    migration: DisabledDto,
    go_away: GracefulDto,
    fallback: NoneDto,
    unsupported: Vec<String>,
    limits: H3LimitsDto,
    blocked_reason: Option<BlockedReasonDto>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct H3LimitsDto {
    max_handshakes_and_connections: usize,
    max_bidirectional_streams: u32,
    max_unidirectional_streams: u32,
    max_field_section_bytes: u64,
    max_request_body_bytes: u64,
    max_response_body_bytes: u64,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Deserialize, JsonSchema, Serialize)]
        #[serde(rename_all = "snake_case")]
        #[allow(clippy::enum_variant_names)]
        enum $name {
            $($variant),+
        }
    };
}

string_enum!(RuntimeModeDto { Direct, Supervised });
string_enum!(DescriptorAdoptionStatusDto {
    Negotiated,
    NotUsed
});
string_enum!(CapabilityStatusDto {
    Active,
    Unconfigured,
    Blocked
});
string_enum!(BlockedReasonDto {
    ListenerRuntimeFailed,
    ListenerStopped,
    ListenerNotListening,
});
string_enum!(UdpTransportDto { Udp });
string_enum!(QuicTransportDto { Quic });
string_enum!(GracefulDto { Graceful });
string_enum!(NoneDto { None });
string_enum!(DisabledDto { Disabled });

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MonitoringResponse {
    sampled_at_unix_ms: u64,
    uptime_ms: u64,
    generation_age_ms: u64,
    process: ProcessDto,
    host: HostDto,
    traffic: TrafficDto,
    listeners: Vec<ListenerDto>,
    upstream_pools: Vec<PoolDto>,
    transport_operations: Vec<TransportOperationDto>,
    access_records: Vec<AccessRecordDto>,
    certbot_certificates: Vec<CertbotCertificateDto>,
    certbot_watcher: Option<CertificateWatcherDto>,
    acme_managed_certificates: Vec<AcmeManagedCertificateDto>,
    direct_file_certificates: Vec<DirectFileCertificateDto>,
    direct_file_watcher: Option<CertificateWatcherDto>,
    rtmp: RtmpMonitoringDto,
}

impl MonitoringResponse {
    pub(crate) fn project(
        runtime: RuntimeSnapshot,
        snapshot: &RtmpCatalogSnapshot,
        recording_supported: bool,
    ) -> Option<Self> {
        Some(Self {
            sampled_at_unix_ms: runtime.sampled_at_unix_ms,
            uptime_ms: runtime.uptime_ms,
            generation_age_ms: runtime.generation_age_ms,
            process: runtime.process.into(),
            host: runtime.host.into(),
            traffic: runtime.traffic.into(),
            listeners: runtime.listeners.into_iter().map(Into::into).collect(),
            upstream_pools: runtime.upstream_pools.into_iter().map(Into::into).collect(),
            transport_operations: runtime
                .transport_operations
                .into_iter()
                .map(Into::into)
                .collect(),
            access_records: runtime.access_records.into_iter().map(Into::into).collect(),
            certbot_certificates: runtime
                .certbot_certificates
                .into_iter()
                .map(Into::into)
                .collect(),
            certbot_watcher: runtime.certbot_watcher.map(Into::into),
            acme_managed_certificates: runtime
                .acme_managed_certificates
                .into_iter()
                .map(Into::into)
                .collect(),
            direct_file_certificates: runtime
                .direct_file_certificates
                .into_iter()
                .map(Into::into)
                .collect(),
            direct_file_watcher: runtime.direct_file_watcher.map(Into::into),
            rtmp: RtmpMonitoringDto::project(snapshot, recording_supported)?,
        })
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessDto {
    active_connections: u64,
    administrative_state: AdministrativeStateDto,
    status: ComponentStatusDto,
    cpu_percent: Option<f64>,
    max_connections: Option<u64>,
    rejected_connections: DecimalCounter,
    retry_attempts: DecimalCounter,
    resident_memory_bytes: Option<u64>,
    virtual_memory_bytes: Option<u64>,
    thread_count: Option<u64>,
    open_file_descriptors: Option<u64>,
}

impl From<ProcessSnapshot> for ProcessDto {
    fn from(snapshot: ProcessSnapshot) -> Self {
        Self {
            active_connections: snapshot.active_connections,
            administrative_state: snapshot.administrative_state.into(),
            status: snapshot.status.into(),
            cpu_percent: snapshot.cpu_percent,
            max_connections: snapshot.max_connections,
            rejected_connections: snapshot.rejected_connections.into(),
            retry_attempts: snapshot.retry_attempts.into(),
            resident_memory_bytes: snapshot.resident_memory_bytes,
            virtual_memory_bytes: snapshot.virtual_memory_bytes,
            thread_count: snapshot.thread_count,
            open_file_descriptors: snapshot.open_file_descriptors,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostDto {
    status: ComponentStatusDto,
    load_average_1m: Option<f64>,
    load_average_5m: Option<f64>,
    load_average_15m: Option<f64>,
    total_memory_bytes: Option<u64>,
    available_memory_bytes: Option<u64>,
}

impl From<HostSnapshot> for HostDto {
    fn from(snapshot: HostSnapshot) -> Self {
        Self {
            status: snapshot.status.into(),
            load_average_1m: snapshot.load_average_1m,
            load_average_5m: snapshot.load_average_5m,
            load_average_15m: snapshot.load_average_15m,
            total_memory_bytes: snapshot.total_memory_bytes,
            available_memory_bytes: snapshot.available_memory_bytes,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrafficDto {
    accepted_connections: DecimalCounter,
    rejected_connections: DecimalCounter,
    active_connections: u64,
    bytes_received: DecimalCounter,
    bytes_sent: DecimalCounter,
}

impl From<TrafficSnapshot> for TrafficDto {
    fn from(snapshot: TrafficSnapshot) -> Self {
        Self {
            accepted_connections: snapshot.accepted_connections.into(),
            rejected_connections: snapshot.rejected_connections.into(),
            active_connections: snapshot.active_connections,
            bytes_received: snapshot.bytes_received.into(),
            bytes_sent: snapshot.bytes_sent.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportOperationDto {
    transport: ObservedTransportDto,
    outcomes: Vec<TransportOperationCountDto>,
    latency: LatencyDto,
}

impl From<TransportOperationSnapshot> for TransportOperationDto {
    fn from(snapshot: TransportOperationSnapshot) -> Self {
        Self {
            transport: snapshot.transport.into(),
            outcomes: snapshot
                .outcomes
                .into_vec()
                .into_iter()
                .map(Into::into)
                .collect(),
            latency: snapshot.latency.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct TransportOperationCountDto {
    outcome: TransportOutcomeDto,
    count: DecimalCounter,
}

impl From<TransportOperationCountSnapshot> for TransportOperationCountDto {
    fn from(snapshot: TransportOperationCountSnapshot) -> Self {
        Self {
            outcome: snapshot.outcome.into(),
            count: snapshot.count.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccessRecordDto {
    timestamp_unix_ms: u64,
    correlation_id: String,
    listener: String,
    transport: ObservedTransportDto,
    outcome: TransportOutcomeDto,
    duration_ms: DecimalCounter,
    bytes_received: DecimalCounter,
    bytes_sent: DecimalCounter,
}

impl From<AccessRecord> for AccessRecordDto {
    fn from(record: AccessRecord) -> Self {
        Self {
            timestamp_unix_ms: record.timestamp_unix_ms,
            correlation_id: record.correlation_id,
            listener: record.listener,
            transport: record.transport.into(),
            outcome: record.outcome.into(),
            duration_ms: record.duration_ms.into(),
            bytes_received: record.bytes_received.into(),
            bytes_sent: record.bytes_sent.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct CertbotCertificateDto {
    name: String,
    active_archive_revision: u64,
    active_content_revision: String,
    expires_at: String,
    last_outcome: Option<String>,
    last_error_code: Option<String>,
}

impl From<CertbotCertificateSnapshot> for CertbotCertificateDto {
    fn from(snapshot: CertbotCertificateSnapshot) -> Self {
        Self {
            name: snapshot.name,
            active_archive_revision: snapshot.active_archive_revision,
            active_content_revision: snapshot.active_content_revision,
            expires_at: snapshot.expires_at,
            last_outcome: snapshot.last_outcome,
            last_error_code: snapshot.last_error_code,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectFileCertificateDto {
    name: String,
    active_content_revision: String,
    expires_at: String,
    last_outcome: Option<String>,
    last_error_code: Option<String>,
}

impl From<DirectFileCertificateSnapshot> for DirectFileCertificateDto {
    fn from(snapshot: DirectFileCertificateSnapshot) -> Self {
        Self {
            name: snapshot.name,
            active_content_revision: snapshot.active_content_revision,
            expires_at: snapshot.expires_at,
            last_outcome: snapshot.last_outcome,
            last_error_code: snapshot.last_error_code,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcmeManagedCertificateDto {
    name: String,
    directory_url: String,
    disk_revision: String,
    active_revision: String,
    expires_at: String,
    not_before_unix_seconds: Option<u64>,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    last_outcome: Option<String>,
    last_error_code: Option<String>,
    renewal_information_status: String,
    dns_provider: Option<String>,
    dns_provider_deployment: Option<String>,
    dns_provider_health: Option<String>,
    dns_cleanup_status: String,
}

impl From<AcmeManagedCertificateSnapshot> for AcmeManagedCertificateDto {
    fn from(snapshot: AcmeManagedCertificateSnapshot) -> Self {
        Self {
            name: snapshot.name,
            directory_url: snapshot.directory_url,
            disk_revision: snapshot.disk_revision,
            active_revision: snapshot.active_revision,
            expires_at: snapshot.expires_at,
            not_before_unix_seconds: snapshot.not_before_unix_seconds,
            not_after_unix_seconds: snapshot.not_after_unix_seconds,
            next_action_unix_seconds: snapshot.next_action_unix_seconds,
            last_outcome: snapshot.last_outcome,
            last_error_code: snapshot.last_error_code,
            renewal_information_status: snapshot.renewal_information_status,
            dns_provider: snapshot.dns_provider,
            dns_provider_deployment: snapshot.dns_provider_deployment,
            dns_provider_health: snapshot.dns_provider_health,
            dns_cleanup_status: snapshot.dns_cleanup_status,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct CertificateWatcherDto {
    health: CertbotWatcherHealthDto,
    coalesced_events: DecimalCounter,
    ignored_access_events: DecimalCounter,
    backend_errors: DecimalCounter,
    watch_recoveries: DecimalCounter,
    watch_refreshes: DecimalCounter,
    rescans: DecimalCounter,
    periodic_rescans: DecimalCounter,
    reconciliation_failures: DecimalCounter,
}

impl From<CertbotWatcherSnapshot> for CertificateWatcherDto {
    fn from(snapshot: CertbotWatcherSnapshot) -> Self {
        Self::from_parts(
            snapshot.health,
            snapshot.coalesced_events,
            snapshot.ignored_access_events,
            snapshot.backend_errors,
            snapshot.watch_recoveries,
            snapshot.watch_refreshes,
            snapshot.rescans,
            snapshot.periodic_rescans,
            snapshot.reconciliation_failures,
        )
    }
}

impl From<DirectFileWatcherSnapshot> for CertificateWatcherDto {
    fn from(snapshot: DirectFileWatcherSnapshot) -> Self {
        Self::from_parts(
            snapshot.health,
            snapshot.coalesced_events,
            snapshot.ignored_access_events,
            snapshot.backend_errors,
            snapshot.watch_recoveries,
            snapshot.watch_refreshes,
            snapshot.rescans,
            snapshot.periodic_rescans,
            snapshot.reconciliation_failures,
        )
    }
}

impl CertificateWatcherDto {
    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        health: CertbotWatcherHealth,
        coalesced_events: u64,
        ignored_access_events: u64,
        backend_errors: u64,
        watch_recoveries: u64,
        watch_refreshes: u64,
        rescans: u64,
        periodic_rescans: u64,
        reconciliation_failures: u64,
    ) -> Self {
        Self {
            health: health.into(),
            coalesced_events: coalesced_events.into(),
            ignored_access_events: ignored_access_events.into(),
            backend_errors: backend_errors.into(),
            watch_recoveries: watch_recoveries.into(),
            watch_refreshes: watch_refreshes.into(),
            rescans: rescans.into(),
            periodic_rescans: periodic_rescans.into(),
            reconciliation_failures: reconciliation_failures.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpMonitoringDto {
    active_streams: u64,
    publishers: u64,
    subscribers: u64,
    media_payload_bytes_received: DecimalCounter,
    recording_supported: bool,
    manual_recording: bool,
    recorder_bytes_written: DecimalCounter,
    recorder_segments_started: DecimalCounter,
    recorder_segments_completed: DecimalCounter,
    recorder_discontinuities: DecimalCounter,
    relay_connection_attempts: DecimalCounter,
    relay_connections: DecimalCounter,
    relay_reconnects: DecimalCounter,
    relay_dns_refresh_attempts: DecimalCounter,
    relay_dns_refresh_successes: DecimalCounter,
    relay_dns_refresh_failures: DecimalCounter,
    relay_events_sent: DecimalCounter,
    relay_events_dropped: DecimalCounter,
    relay_payload_bytes_sent: DecimalCounter,
    access_log: RtmpAccessLogMonitoringDto,
    relays: Vec<RelayMonitoringRowDto>,
    recorders: Vec<RecorderMonitoringRowDto>,
}

impl RtmpMonitoringDto {
    #[allow(clippy::too_many_lines)]
    fn project(snapshot: &RtmpCatalogSnapshot, recording_supported: bool) -> Option<Self> {
        let active_streams = u64::try_from(snapshot.streams.len()).ok()?;
        let mut publishers = 0_u64;
        let mut subscribers = 0_u64;
        let mut media_payload_bytes_received = 0_u64;
        let mut recorder_bytes_written = 0_u64;
        let mut recorder_segments_started = 0_u64;
        let mut recorder_segments_completed = 0_u64;
        let mut recorder_discontinuities = 0_u64;
        let mut relay_connection_attempts = 0_u64;
        let mut relay_connections = 0_u64;
        let mut relay_reconnects = 0_u64;
        let mut relay_dns_refresh_attempts = 0_u64;
        let mut relay_dns_refresh_successes = 0_u64;
        let mut relay_dns_refresh_failures = 0_u64;
        let mut relay_events_sent = 0_u64;
        let mut relay_events_dropped = 0_u64;
        let mut relay_payload_bytes_sent = 0_u64;
        let mut relays = Vec::new();
        let mut recorders = Vec::new();
        let access_log = crate::logging::rtmp_access_log_snapshot();
        for stream in &snapshot.streams {
            if stream.publisher.is_some() {
                publishers = publishers.checked_add(1)?;
            }
            subscribers = subscribers.checked_add(u64::try_from(stream.subscriber_count).ok()?)?;
            media_payload_bytes_received = media_payload_bytes_received
                .checked_add(stream.media.audio.payload_bytes_received)?
                .checked_add(stream.media.video.payload_bytes_received)?;
            for relay in &stream.relays {
                relay_connection_attempts =
                    relay_connection_attempts.checked_add(relay.status.connection_attempts)?;
                relay_connections = relay_connections.checked_add(relay.status.connections)?;
                relay_reconnects = relay_reconnects.checked_add(relay.status.reconnects)?;
                relay_dns_refresh_attempts =
                    relay_dns_refresh_attempts.checked_add(relay.status.dns_refresh_attempts)?;
                relay_dns_refresh_successes =
                    relay_dns_refresh_successes.checked_add(relay.status.dns_refresh_successes)?;
                relay_dns_refresh_failures =
                    relay_dns_refresh_failures.checked_add(relay.status.dns_refresh_failures)?;
                relay_events_sent = relay_events_sent.checked_add(relay.status.events_sent)?;
                relay_events_dropped =
                    relay_events_dropped.checked_add(relay.status.events_dropped)?;
                relay_payload_bytes_sent =
                    relay_payload_bytes_sent.checked_add(relay.status.payload_bytes_sent)?;
                relays.push(RelayMonitoringRowDto {
                    stream_id: stream.id.to_string(),
                    relay_id: relay.id.to_string(),
                    address: relay.status.destination.address.to_string(),
                    application: relay.status.destination.application.clone(),
                    stream_name: relay.status.destination.stream_name.clone(),
                    phase: relay.status.phase.into(),
                    last_failure: relay.status.last_failure.map(Into::into),
                    queue_messages: relay.status.queue_messages,
                    queue_bytes: u64::try_from(relay.status.queue_bytes).ok()?.into(),
                    connection_attempts: relay.status.connection_attempts.into(),
                    connections: relay.status.connections.into(),
                    reconnects: relay.status.reconnects.into(),
                    dns_refresh_attempts: relay.status.dns_refresh_attempts.into(),
                    dns_refresh_successes: relay.status.dns_refresh_successes.into(),
                    dns_refresh_failures: relay.status.dns_refresh_failures.into(),
                    last_dns_refresh_failure: relay.status.last_dns_refresh_failure.map(Into::into),
                    events_sent: relay.status.events_sent.into(),
                    events_dropped: relay.status.events_dropped.into(),
                    payload_bytes_sent: relay.status.payload_bytes_sent.into(),
                });
            }
            for recorder in &stream.recorders {
                recorder_bytes_written =
                    recorder_bytes_written.checked_add(recorder.bytes_written)?;
                recorder_segments_started =
                    recorder_segments_started.checked_add(recorder.segments_started)?;
                recorder_segments_completed =
                    recorder_segments_completed.checked_add(recorder.segments_completed)?;
                recorder_discontinuities =
                    recorder_discontinuities.checked_add(recorder.discontinuities)?;
                recorders.push(RecorderMonitoringRowDto {
                    stream_id: stream.id.to_string(),
                    recorder_id: recorder.id.to_string(),
                    name: recorder.name.clone(),
                    manual: recorder.manual,
                    phase: recorder.phase.into(),
                    bytes_written: recorder.bytes_written.into(),
                    segments_started: recorder.segments_started.into(),
                    segments_completed: recorder.segments_completed.into(),
                    discontinuities: recorder.discontinuities.into(),
                    current_relative_name: recorder.current_relative_name.clone(),
                    last_completed_relative_name: recorder.last_completed_relative_name.clone(),
                    recoverable_partial_name: recorder.recoverable_partial_name.clone(),
                    published_but_not_durable_relative_name: recorder
                        .published_but_not_durable_relative_name
                        .clone(),
                });
            }
        }
        Some(Self {
            active_streams,
            publishers,
            subscribers,
            media_payload_bytes_received: media_payload_bytes_received.into(),
            recording_supported,
            manual_recording: snapshot.capabilities.manual_recording,
            recorder_bytes_written: recorder_bytes_written.into(),
            recorder_segments_started: recorder_segments_started.into(),
            recorder_segments_completed: recorder_segments_completed.into(),
            recorder_discontinuities: recorder_discontinuities.into(),
            relay_connection_attempts: relay_connection_attempts.into(),
            relay_connections: relay_connections.into(),
            relay_reconnects: relay_reconnects.into(),
            relay_dns_refresh_attempts: relay_dns_refresh_attempts.into(),
            relay_dns_refresh_successes: relay_dns_refresh_successes.into(),
            relay_dns_refresh_failures: relay_dns_refresh_failures.into(),
            relay_events_sent: relay_events_sent.into(),
            relay_events_dropped: relay_events_dropped.into(),
            relay_payload_bytes_sent: relay_payload_bytes_sent.into(),
            access_log: RtmpAccessLogMonitoringDto {
                queue_capacity: crate::logging::RTMP_ACCESS_LOG_QUEUE_CAPACITY,
                queue_depth: access_log.queue_depth.into(),
                enqueued: access_log.enqueued.into(),
                written: access_log.written.into(),
                dropped: access_log.dropped.into(),
                queue_saturated: access_log.queue_saturated.into(),
                write_failures: access_log.write_failures.into(),
            },
            relays,
            recorders,
        })
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpAccessLogMonitoringDto {
    queue_capacity: u64,
    queue_depth: DecimalCounter,
    enqueued: DecimalCounter,
    written: DecimalCounter,
    dropped: DecimalCounter,
    queue_saturated: DecimalCounter,
    write_failures: DecimalCounter,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayMonitoringRowDto {
    stream_id: String,
    relay_id: String,
    address: String,
    application: String,
    stream_name: String,
    phase: RelayPhaseDto,
    last_failure: Option<RelayFailureDto>,
    queue_messages: usize,
    queue_bytes: DecimalCounter,
    connection_attempts: DecimalCounter,
    connections: DecimalCounter,
    reconnects: DecimalCounter,
    dns_refresh_attempts: DecimalCounter,
    dns_refresh_successes: DecimalCounter,
    dns_refresh_failures: DecimalCounter,
    last_dns_refresh_failure: Option<RelayDnsRefreshFailureDto>,
    events_sent: DecimalCounter,
    events_dropped: DecimalCounter,
    payload_bytes_sent: DecimalCounter,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecorderMonitoringRowDto {
    stream_id: String,
    recorder_id: String,
    name: Option<String>,
    manual: bool,
    phase: RecorderPhaseDto,
    bytes_written: DecimalCounter,
    segments_started: DecimalCounter,
    segments_completed: DecimalCounter,
    discontinuities: DecimalCounter,
    current_relative_name: Option<String>,
    last_completed_relative_name: Option<String>,
    recoverable_partial_name: Option<String>,
    published_but_not_durable_relative_name: Option<String>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditStatusDto {
    state: AuditComponentStateDto,
    persistent: bool,
    degraded: bool,
    record_count: u64,
    bytes: u64,
    rotated_files: u64,
    max_records: u64,
    max_record_bytes: u64,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_rotated_files: u64,
    write_failures: u64,
    corrupt_records: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<&'static str>,
}

impl From<AuditStatus> for AuditStatusDto {
    fn from(status: AuditStatus) -> Self {
        Self {
            state: status.state.into(),
            persistent: status.persistent,
            degraded: status.degraded,
            record_count: status.record_count,
            bytes: status.bytes,
            rotated_files: status.rotated_files,
            max_records: status.max_records,
            max_record_bytes: status.max_record_bytes,
            max_file_bytes: status.max_file_bytes,
            max_total_bytes: status.max_total_bytes,
            max_rotated_files: status.max_rotated_files,
            write_failures: status.write_failures,
            corrupt_records: status.corrupt_records,
            last_error: status.last_error,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentStatusDto {
    state: ComponentStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl From<ComponentStatus> for ComponentStatusDto {
    fn from(status: ComponentStatus) -> Self {
        Self {
            state: status.state.into(),
            reason: status.reason,
        }
    }
}

fn generation_component(generation: Option<&GenerationStatus>) -> ComponentStatusDto {
    if let Some(status) = generation.filter(|status| status.degraded) {
        ComponentStatusDto {
            state: ComponentStateDto::Degraded,
            reason: status.last_failure,
        }
    } else if generation.is_some_and(|status| status.active_revision.is_some()) {
        ComponentStatusDto {
            state: ComponentStateDto::Healthy,
            reason: None,
        }
    } else {
        ComponentStatusDto {
            state: ComponentStateDto::Degraded,
            reason: Some("active_generation_unavailable"),
        }
    }
}

macro_rules! projected_enum {
    ($name:ident from $domain:ty { $($source:path => $target:ident),+ $(,)? }) => {
        #[derive(JsonSchema, Serialize)]
        #[serde(rename_all = "snake_case")]
        enum $name {
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
enum RecorderPhaseDto {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use oxiroute_rtmp::{
        PreparedRtmpRuntimeSet, RtmpPrepareContext, RtmpPrepareMode, RtmpRuntimeSet,
    };
    use schemars::generate::SchemaSettings;
    use serde_json::{Value, json};

    use super::*;
    use crate::RuntimeMetrics;

    fn schema<T: JsonSchema>() -> Value {
        let generator = SchemaSettings::draft2020_12().into_generator();
        serde_json::to_value(generator.into_root_schema_for::<T>()).expect("response schema")
    }

    fn empty_rtmp_runtime() -> RtmpRuntimeSet {
        PreparedRtmpRuntimeSet::prepare(
            [],
            &RtmpPrepareContext::new(RtmpPrepareMode::Activation, []),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .expect("empty RTMP preparation")
        .start(std::time::Instant::now() + std::time::Duration::from_secs(1))
        .expect("empty RTMP runtime")
    }

    #[test]
    fn monitoring_projection_preserves_the_runtime_v1_wire_object() {
        let runtime = RuntimeMetrics::new().snapshot().expect("runtime snapshot");
        let legacy_runtime = serde_json::to_value(&runtime).expect("legacy runtime JSON");
        let control = empty_rtmp_runtime().control();
        let projected = MonitoringResponse::project(runtime, &control.catalog_snapshot(), false)
            .expect("monitoring projection");
        let projected = serde_json::to_value(projected).expect("projected monitoring JSON");

        for (key, value) in legacy_runtime.as_object().expect("runtime object") {
            assert_eq!(&projected[key], value, "runtime field {key}");
        }
        assert_eq!(projected["rtmp"]["relays"], json!([]));
        assert_eq!(projected["rtmp"]["recorders"], json!([]));
    }

    #[test]
    fn monitoring_projection_owns_every_runtime_snapshot_field() {
        let runtime = RuntimeMetrics::new().snapshot().expect("runtime snapshot");
        let control = empty_rtmp_runtime().control();
        let value = serde_json::to_value(
            MonitoringResponse::project(runtime, &control.catalog_snapshot(), false)
                .expect("monitoring projection"),
        )
        .expect("monitoring JSON");
        let actual = value
            .as_object()
            .expect("monitoring object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = [
            "accessRecords",
            "acmeManagedCertificates",
            "certbotCertificates",
            "certbotWatcher",
            "directFileCertificates",
            "directFileWatcher",
            "generationAgeMs",
            "host",
            "listeners",
            "process",
            "rtmp",
            "sampledAtUnixMs",
            "traffic",
            "transportOperations",
            "upstreamPools",
            "uptimeMs",
        ]
        .into_iter()
        .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn observability_schemas_are_structural_and_keep_decimal_counters_as_strings() {
        for response in [
            schema::<StatusResponse>(),
            schema::<ReadinessResponse>(),
            schema::<CapabilitiesResponse>(),
            schema::<MonitoringResponse>(),
        ] {
            let encoded = response.to_string();
            assert!(!encoded.contains("serde_json::Value"));
            assert!(!encoded.contains("\"additionalProperties\":true"));
        }
        let monitoring = schema::<MonitoringResponse>();
        assert_eq!(monitoring["$defs"]["DecimalCounter"]["type"], "string");
        assert_eq!(
            monitoring["$defs"]["RelayMonitoringRowDto"]["properties"]["queueBytes"]["$ref"],
            "#/$defs/DecimalCounter"
        );
        assert_eq!(
            monitoring["$defs"]["RecorderMonitoringRowDto"]["properties"]["bytesWritten"]["$ref"],
            "#/$defs/DecimalCounter"
        );
    }

    #[test]
    fn capability_projection_matches_the_existing_v1_golden_shape() {
        let value = serde_json::to_value(
            CapabilitiesResponse::project(&[], RuntimeMode::Direct)
                .expect("capabilities projection"),
        )
        .expect("capabilities JSON");
        assert_eq!(
            value,
            crate::http3::capability_snapshot(&[], RuntimeMode::Direct)
        );
    }

    #[test]
    fn public_observability_shapes_do_not_expose_secret_bearing_fields() {
        let schemas = [schema::<StatusResponse>(), schema::<MonitoringResponse>()]
            .into_iter()
            .map(|schema| schema.to_string().to_ascii_lowercase())
            .collect::<String>();
        for forbidden in ["token", "password", "privatekey", "authorization"] {
            assert!(!schemas.contains(forbidden), "schema exposed {forbidden}");
        }
    }
}
