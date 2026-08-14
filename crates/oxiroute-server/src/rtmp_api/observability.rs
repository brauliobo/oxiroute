use oxiroute_rtmp::RtmpControlHandle;
use serde::Serialize;
use serde_json::Value;

use super::ApiResponse;
use super::dto::{
    CapabilitiesResponse, MonitoringResponse, ReadinessResponse, StatusResponse, TopologyResponse,
};
use crate::{
    GenerationManager, RuntimeMetrics, TopologySnapshot, prometheus::render_prometheus_control,
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
    control: &RtmpControlHandle,
    topology: &TopologySnapshot,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    match route {
        Route::Topology => topology_response(metrics, topology),
        Route::Monitoring => monitoring_response(metrics, control),
        Route::Metrics => metrics_response(metrics, control, generations),
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
    let generation = generations.map(GenerationManager::status);
    let audit = crate::operational_event::audit_status();
    let response = match StatusResponse::project(
        runtime,
        generation.as_ref(),
        audit,
        topology,
        metrics.supervision_mode(),
    ) {
        Ok(response) => response,
        Err(error) => {
            return ApiResponse::error(
                500,
                "status_projection_failed",
                format!("could not project runtime status: {error}"),
            );
        }
    };
    serialized_response(
        200,
        &response,
        "status_serialization_failed",
        "could not serialize runtime status",
    )
}

fn capabilities_response(metrics: &RuntimeMetrics) -> ApiResponse {
    match metrics.snapshot() {
        Ok(runtime) => {
            let response =
                match CapabilitiesResponse::project(&runtime.listeners, metrics.supervision_mode())
                {
                    Ok(response) => response,
                    Err(error) => {
                        return ApiResponse::error(
                            500,
                            "capabilities_projection_failed",
                            format!("could not project runtime capabilities: {error}"),
                        );
                    }
                };
            serialized_response(
                200,
                &response,
                "capabilities_serialization_failed",
                "could not serialize runtime capabilities",
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
    let response = ReadinessResponse::project(&runtime, generations.map(GenerationManager::status));
    serialized_response(
        if response.ready() { 200 } else { 503 },
        &response,
        "readiness_serialization_failed",
        "could not serialize runtime readiness",
    )
}

fn serialized_response<T: Serialize>(
    status: u16,
    response: &T,
    error_code: &'static str,
    error_message: &'static str,
) -> ApiResponse {
    match serde_json::to_value(response) {
        Ok(value) => ApiResponse::json(status, &value),
        Err(_) => ApiResponse::error(500, error_code, error_message),
    }
}

fn metrics_response(
    metrics: &RuntimeMetrics,
    control: &RtmpControlHandle,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    let Some(generations) = generations else {
        return ApiResponse::error(
            503,
            "metrics_unavailable",
            "generation state is unavailable",
        );
    };
    match render_prometheus_control(metrics, control, generations) {
        Ok(body) => ApiResponse::bytes(
            200,
            body.into_bytes(),
            "text/plain; version=0.0.4; charset=utf-8",
        ),
        Err(_) => ApiResponse::error(503, "metrics_unavailable", "metrics are unavailable"),
    }
}

pub(super) fn candidate_topology(topology: &TopologySnapshot, now_unix_ms: u64) -> Value {
    let response = TopologyResponse::candidate(topology, now_unix_ms)
        .expect("compiled candidate topology has valid API attributes");
    serde_json::to_value(response).expect("topology DTO serialization cannot fail")
}

fn monitoring_response(metrics: &RuntimeMetrics, control: &RtmpControlHandle) -> ApiResponse {
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
    let Some(response) =
        MonitoringResponse::project(runtime, &control.catalog_snapshot(), recording_supported)
    else {
        return ApiResponse::error(
            500,
            "monitoring_overflow",
            "RTMP monitoring totals exceed the supported range",
        );
    };
    match serde_json::to_value(response) {
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
    match TopologyResponse::active(topology, &runtime) {
        Ok(response) => serialized_response(
            200,
            &response,
            "topology_serialization_failed",
            "could not serialize active runtime topology",
        ),
        Err(error) => ApiResponse::error(
            500,
            "topology_serialization_failed",
            format!("could not serialize active runtime topology: {error}"),
        ),
    }
}
