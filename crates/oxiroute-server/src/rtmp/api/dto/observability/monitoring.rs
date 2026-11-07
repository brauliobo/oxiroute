#[allow(clippy::wildcard_imports)]
use super::*;

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
        let access_log = crate::observability::logging::rtmp_access_log_snapshot();
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
                queue_capacity: crate::observability::logging::RTMP_ACCESS_LOG_QUEUE_CAPACITY,
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
