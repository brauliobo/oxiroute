use oxiroute_rtmp::{RtmpCatalogSnapshot, RtmpRegistry};
use serde::Serialize;
use serde_json::{Value, json};

use super::ApiResponse;
use crate::{RuntimeMetrics, RuntimeSnapshot, TOPOLOGY_SCHEMA_VERSION, TopologySnapshot};

#[derive(Clone, Copy)]
pub(super) enum Route {
    Topology,
    Monitoring,
}

pub(super) fn match_route(path: &str) -> Option<Route> {
    match path {
        "/api/v1/topology" => Some(Route::Topology),
        "/api/v1/monitoring" => Some(Route::Monitoring),
        _ => None,
    }
}

pub(super) fn handle(
    route: Route,
    method: &str,
    metrics: &RuntimeMetrics,
    registry: &RtmpRegistry,
    topology: &TopologySnapshot,
) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    match route {
        Route::Topology => topology_response(metrics, topology),
        Route::Monitoring => monitoring_response(metrics, registry),
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
    media_payload_bytes_received: u64,
    recording_supported: bool,
    manual_recording: bool,
    recorder_bytes_written: u64,
    recorder_segments_started: u64,
    recorder_segments_completed: u64,
    recorder_discontinuities: u64,
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
    let mut recorders = Vec::new();
    for stream in &snapshot.streams {
        if stream.publisher.is_some() {
            publishers = publishers.checked_add(1)?;
        }
        subscribers = subscribers.checked_add(u64::try_from(stream.subscriber_count).ok()?)?;
        media_payload_bytes_received = media_payload_bytes_received
            .checked_add(stream.media.audio.payload_bytes_received)?
            .checked_add(stream.media.video.payload_bytes_received)?;
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
        recorders,
    })
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
