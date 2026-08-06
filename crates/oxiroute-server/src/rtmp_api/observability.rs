use oxiroute_rtmp::{RtmpCatalogSnapshot, RtmpRegistry};
use serde::Serialize;
use serde_json::{Value, json};

use super::ApiResponse;
use crate::{
    GenerationManager, RuntimeMetrics, RuntimeSnapshot, TOPOLOGY_SCHEMA_VERSION, TopologyNodeKind,
    TopologySnapshot, render_prometheus,
};

#[derive(Clone, Copy)]
pub(super) enum Route {
    Topology,
    Monitoring,
    Metrics,
    Readiness,
    Status,
    Capabilities,
}

pub(super) fn match_route(path: &str) -> Option<Route> {
    match path {
        "/api/v1/topology" => Some(Route::Topology),
        "/api/v1/monitoring" => Some(Route::Monitoring),
        "/api/v1/status" => Some(Route::Status),
        "/api/v1/capabilities" => Some(Route::Capabilities),
        "/ready" => Some(Route::Readiness),
        "/metrics" => Some(Route::Metrics),
        _ => None,
    }
}

pub(super) fn handle(
    route: Route,
    method: &str,
    metrics: &RuntimeMetrics,
    registry: &RtmpRegistry,
    topology: &TopologySnapshot,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    match route {
        Route::Topology => topology_response(metrics, topology),
        Route::Monitoring => monitoring_response(metrics, registry),
        Route::Metrics => metrics_response(metrics, registry, generations),
        Route::Readiness => readiness_response(metrics, generations),
        Route::Status => status_response(metrics, generations, topology),
        Route::Capabilities => capabilities_response(metrics),
    }
}

fn status_response(
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
    topology: &TopologySnapshot,
) -> ApiResponse {
    let Ok(runtime) = metrics.snapshot() else {
        return ApiResponse::error(
            503,
            "status_unavailable",
            "runtime status could not be sampled",
        );
    };
    let listeners = runtime.listeners.clone();
    let generation = generations.map(GenerationManager::status);
    let audit = crate::operational_event::audit_status();
    let tls_profiles = topology
        .nodes()
        .iter()
        .filter(|node| node.kind == TopologyNodeKind::TlsProfile)
        .map(|node| {
            json!({
                "name": node.name,
                "clientAuth": node.attributes["clientAuth"],
            })
        })
        .collect::<Vec<_>>();
    ApiResponse::json(
        200,
        &json!({
            "schemaVersion": 1,
            "buildVersion": crate::cli::BUILD_VERSION,
            "diskRevision": generation.as_ref().and_then(|status| status.disk_revision.as_ref()),
            "candidateRevision": generation.as_ref().and_then(|status| status.candidate_revision.as_ref()),
            "activeRevision": generation.as_ref().and_then(|status| status.active_revision.as_ref()),
            "previousRevision": generation.as_ref().and_then(|status| status.previous_revision.as_ref()),
            "degraded": generation.as_ref().is_none_or(|status| status.degraded),
            "activeGenerationAgeMs": runtime.generation_age_ms,
            "components": {
                "process": runtime.process.status,
                "host": runtime.host.status,
                "generation": generation_component_status(generation.as_ref()),
                "audit": audit.clone(),
            },
            "certificates": {
                "certbot": runtime.certbot_certificates,
                "acmeManaged": runtime.acme_managed_certificates,
                "directFiles": runtime.direct_file_certificates,
            },
            "audit": audit,
            "capabilities": crate::http3::capability_snapshot(&listeners, metrics.supervision_mode()),
            "listeners": listeners,
            "tlsProfiles": tls_profiles,
        }),
    )
}

fn generation_component_status(
    generation: Option<&crate::GenerationStatus>,
) -> serde_json::Value {
    match generation {
        Some(status) if status.degraded => json!({
            "state": "degraded",
            "reason": status.last_failure,
        }),
        Some(status) if status.active_revision.is_some() => json!({ "state": "healthy" }),
        Some(_) | None => json!({
            "state": "degraded",
            "reason": "active_generation_unavailable",
        }),
    }
}

fn capabilities_response(metrics: &RuntimeMetrics) -> ApiResponse {
    match metrics.snapshot() {
        Ok(runtime) => {
            ApiResponse::json(
                200,
                &crate::http3::capability_snapshot(&runtime.listeners, metrics.supervision_mode()),
            )
        }
        Err(_) => ApiResponse::error(
            503,
            "capabilities_unavailable",
            "runtime capabilities could not be sampled",
        ),
    }
}

fn readiness_response(
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    let Ok(runtime) = metrics.snapshot() else {
        return ApiResponse::error(503, "not_ready", "runtime metrics are unavailable");
    };
    let generation = generations.map(GenerationManager::status);
    let ready = generation.as_ref().is_some_and(|status| {
        status.active_revision.is_some()
            && !status.degraded
            && runtime.process.administrative_state == crate::AdministrativeState::Ready
            && runtime.listeners.iter().all(|listener| {
                listener.state == crate::ListenerRuntimeState::Listening
                    && listener.administrative_state == crate::AdministrativeState::Ready
            })
    });
    ApiResponse::json(
        if ready { 200 } else { 503 },
        &json!({
            "ready": ready,
            "buildVersion": crate::cli::BUILD_VERSION,
            "activeRevision": generation.and_then(|status| status.active_revision),
        }),
    )
}

fn metrics_response(
    metrics: &RuntimeMetrics,
    registry: &RtmpRegistry,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    let Some(generations) = generations else {
        return ApiResponse::error(
            503,
            "metrics_unavailable",
            "generation state is unavailable",
        );
    };
    match render_prometheus(metrics, registry, generations) {
        Ok(body) => ApiResponse::bytes(
            200,
            body.into_bytes(),
            "text/plain; version=0.0.4; charset=utf-8",
        ),
        Err(_) => ApiResponse::error(503, "metrics_unavailable", "metrics are unavailable"),
    }
}

pub(super) fn candidate_topology(topology: &TopologySnapshot, now_unix_ms: u64) -> Value {
    json!({
        "schemaVersion": TOPOLOGY_SCHEMA_VERSION,
        "state": {
            "config": "candidate",
            "runtime": "not_active",
            "sampledAtUnixMs": now_unix_ms,
        },
        "nodes": topology.nodes(),
        "edges": topology.edges(),
        "overlays": [],
    })
}

#[derive(Serialize)]
struct MonitoringResponse {
    #[serde(flatten)]
    runtime: RuntimeSnapshot,
    rtmp: RtmpMonitoring,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpMonitoring {
    active_streams: u64,
    publishers: u64,
    subscribers: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    media_payload_bytes_received: u64,
    recording_supported: bool,
    manual_recording: bool,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    recorder_bytes_written: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    recorder_segments_started: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    recorder_segments_completed: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    recorder_discontinuities: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    relay_connection_attempts: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    relay_connections: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    relay_reconnects: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    relay_events_sent: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    relay_events_dropped: u64,
    #[serde(serialize_with = "crate::wire::serialize_u64_string")]
    relay_payload_bytes_sent: u64,
    relays: Vec<Value>,
    recorders: Vec<Value>,
}

fn monitoring_response(metrics: &RuntimeMetrics, registry: &RtmpRegistry) -> ApiResponse {
    let runtime = match metrics.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return ApiResponse::error(
                503,
                "monitoring_unavailable",
                format!("could not sample runtime monitoring: {error}"),
            );
        }
    };
    let recording_supported = metrics.rtmp_recording_supported();
    let Some(rtmp) = rtmp_monitoring(&registry.snapshot(), recording_supported) else {
        return ApiResponse::error(
            500,
            "monitoring_overflow",
            "RTMP monitoring totals exceed the supported range",
        );
    };
    match serde_json::to_value(MonitoringResponse { runtime, rtmp }) {
        Ok(value) => ApiResponse::json(200, &value),
        Err(error) => ApiResponse::error(
            500,
            "monitoring_serialization_failed",
            format!("could not serialize runtime monitoring: {error}"),
        ),
    }
}

fn topology_response(metrics: &RuntimeMetrics, topology: &TopologySnapshot) -> ApiResponse {
    let runtime = match metrics.topology_health_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return ApiResponse::error(
                503,
                "topology_unavailable",
                format!("could not sample active runtime topology: {error}"),
            );
        }
    };
    match topology.response_value(&runtime) {
        Ok(value) => ApiResponse::json(200, &value),
        Err(error) => ApiResponse::error(
            500,
            "topology_serialization_failed",
            format!("could not serialize active runtime topology: {error}"),
        ),
    }
}

fn rtmp_monitoring(
    snapshot: &RtmpCatalogSnapshot,
    recording_supported: bool,
) -> Option<RtmpMonitoring> {
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
    let mut relay_events_sent = 0_u64;
    let mut relay_events_dropped = 0_u64;
    let mut relay_payload_bytes_sent = 0_u64;
    let mut relays = Vec::new();
    let mut recorders = Vec::new();
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
            relay_events_sent = relay_events_sent.checked_add(relay.status.events_sent)?;
            relay_events_dropped = relay_events_dropped.checked_add(relay.status.events_dropped)?;
            relay_payload_bytes_sent =
                relay_payload_bytes_sent.checked_add(relay.status.payload_bytes_sent)?;
            relays.push(json!({
                "streamId": stream.id.to_string(),
                "relayId": relay.id.to_string(),
                "address": relay.status.destination.address.to_string(),
                "application": relay.status.destination.application,
                "streamName": relay.status.destination.stream_name,
                "phase": relay_phase(relay.status.phase),
                "lastFailure": relay.status.last_failure.map(relay_failure),
                "queueMessages": relay.status.queue_messages,
                "queueBytes": relay.status.queue_bytes.to_string(),
                "connectionAttempts": relay.status.connection_attempts.to_string(),
                "connections": relay.status.connections.to_string(),
                "reconnects": relay.status.reconnects.to_string(),
                "eventsSent": relay.status.events_sent.to_string(),
                "eventsDropped": relay.status.events_dropped.to_string(),
                "payloadBytesSent": relay.status.payload_bytes_sent.to_string(),
            }));
        }
        for recorder in &stream.recorders {
            recorder_bytes_written = recorder_bytes_written.checked_add(recorder.bytes_written)?;
            recorder_segments_started =
                recorder_segments_started.checked_add(recorder.segments_started)?;
            recorder_segments_completed =
                recorder_segments_completed.checked_add(recorder.segments_completed)?;
            recorder_discontinuities =
                recorder_discontinuities.checked_add(recorder.discontinuities)?;
            recorders.push(json!({
                "streamId": stream.id.to_string(),
                "recorderId": recorder.id.to_string(),
                "name": recorder.name,
                "manual": recorder.manual,
                "phase": recorder_phase(recorder.phase),
                "bytesWritten": recorder.bytes_written.to_string(),
                "segmentsStarted": recorder.segments_started.to_string(),
                "segmentsCompleted": recorder.segments_completed.to_string(),
                "discontinuities": recorder.discontinuities.to_string(),
                "currentRelativeName": recorder.current_relative_name,
                "lastCompletedRelativeName": recorder.last_completed_relative_name,
                "recoverablePartialName": recorder.recoverable_partial_name,
                "publishedButNotDurableRelativeName": recorder.published_but_not_durable_relative_name,
            }));
        }
    }
    Some(RtmpMonitoring {
        active_streams,
        publishers,
        subscribers,
        media_payload_bytes_received,
        recording_supported,
        manual_recording: snapshot.capabilities.manual_recording,
        recorder_bytes_written,
        recorder_segments_started,
        recorder_segments_completed,
        recorder_discontinuities,
        relay_connection_attempts,
        relay_connections,
        relay_reconnects,
        relay_events_sent,
        relay_events_dropped,
        relay_payload_bytes_sent,
        relays,
        recorders,
    })
}

const fn relay_phase(phase: oxiroute_rtmp::RtmpRelayPhase) -> &'static str {
    match phase {
        oxiroute_rtmp::RtmpRelayPhase::Connecting => "connecting",
        oxiroute_rtmp::RtmpRelayPhase::Publishing => "publishing",
        oxiroute_rtmp::RtmpRelayPhase::Pulling => "pulling",
        oxiroute_rtmp::RtmpRelayPhase::Backoff => "backoff",
        oxiroute_rtmp::RtmpRelayPhase::Stopped => "stopped",
    }
}

const fn relay_failure(failure: oxiroute_rtmp::RtmpRelayFailure) -> &'static str {
    match failure {
        oxiroute_rtmp::RtmpRelayFailure::Policy => "policy",
        oxiroute_rtmp::RtmpRelayFailure::Connect => "connect",
        oxiroute_rtmp::RtmpRelayFailure::Handshake => "handshake",
        oxiroute_rtmp::RtmpRelayFailure::Session => "session",
        oxiroute_rtmp::RtmpRelayFailure::Transport => "transport",
        oxiroute_rtmp::RtmpRelayFailure::Source => "source",
        oxiroute_rtmp::RtmpRelayFailure::Thread => "thread",
    }
}

fn recorder_phase(phase: oxiroute_rtmp::RecorderPhase) -> &'static str {
    match phase {
        oxiroute_rtmp::RecorderPhase::Idle => "idle",
        oxiroute_rtmp::RecorderPhase::Starting { .. } => "starting",
        oxiroute_rtmp::RecorderPhase::Recording { .. } => "recording",
        oxiroute_rtmp::RecorderPhase::Stopping { .. } => "stopping",
        oxiroute_rtmp::RecorderPhase::Failed { .. } => "failed",
    }
}
