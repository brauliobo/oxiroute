use std::collections::HashMap;

use oxiroute_rtmp::RtmpControlHandle;
use serde::Serialize;
use serde_json::Value;

use super::ApiResponse;
use super::dto::{
    CapabilitiesResponse, ClientListResponse, ConnectionListResponse, ListenerFacts,
    MonitoringResponse, ReadinessResponse, RequestListResponse, StatusResponse, TopologyResponse,
};
use crate::{
    GenerationManager, RuntimeMetrics, TopologySnapshot,
    config::coordinator::CanonicalConfigCoordinator,
    observability::prometheus::render_prometheus_control,
};

#[derive(Clone, Copy)]
pub(super) enum Route {
    Topology,
    Monitoring,
    Stats,
    Metrics,
    Readiness,
    Status,
    StatusJson,
    Capabilities,
}

pub(super) fn match_route(path: &str) -> Option<Route> {
    match path {
        "/api/v1/topology" => Some(Route::Topology),
        "/api/v1/monitoring" => Some(Route::Monitoring),
        "/api/v1/stats" => Some(Route::Stats),
        "/api/v1/capabilities" => Some(Route::Capabilities),
        "/ready" => Some(Route::Readiness),
        "/status.json" => Some(Route::StatusJson),
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
    coordinator: Option<&CanonicalConfigCoordinator>,
) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    match route {
        Route::Topology => topology_response(metrics, topology),
        Route::Monitoring => monitoring_response(metrics, control),
        Route::Stats => stats_response(generations),
        Route::Metrics => metrics_response(metrics, control, generations),
        Route::Readiness => readiness_response(metrics, generations),
        Route::Status => status_response(metrics, generations, topology, coordinator),
        Route::StatusJson => {
            crate::observability::status::summary::status_json_response(metrics, generations)
        }
        Route::Capabilities => capabilities_response(metrics),
    }
}

fn tracking_registry(
    metrics: &RuntimeMetrics,
) -> Result<&std::sync::Arc<crate::clients::Registry>, ApiResponse> {
    let registry = metrics.clients();
    if registry.settings().enabled {
        Ok(registry)
    } else {
        Err(ApiResponse::error(
            409,
            "client_tracking_disabled",
            "client tracking is disabled by the client_tracking configuration",
        ))
    }
}

fn listener_facts(generations: Option<&GenerationManager>) -> HashMap<String, ListenerFacts> {
    let Some(generation) = generations.and_then(GenerationManager::active) else {
        return HashMap::new();
    };
    generation
        .config()
        .as_draft()
        .listeners
        .iter()
        .map(|listener| {
            let draining = generation
                .traffic_listener_metrics(&listener.name)
                .is_ok_and(|metrics| !metrics.accepting());
            (
                listener.name.clone(),
                ListenerFacts {
                    service: listener.service.clone(),
                    draining,
                },
            )
        })
        .collect()
}

pub(super) fn clients(
    method: &str,
    query: Option<&str>,
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    let registry = match tracking_registry(metrics) {
        Ok(registry) => registry,
        Err(response) => return response,
    };
    let generation = generations.map_or(0, |generations| generations.status().generation);
    match ClientListResponse::current(query, registry, generation) {
        Ok(response) => serialized_response(
            200,
            &response,
            "clients_serialization_failed",
            "could not serialize the client list",
        ),
        Err(message) => ApiResponse::error(400, "invalid_client_query", message),
    }
}

pub(super) fn connections(
    method: &str,
    query: Option<&str>,
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    let registry = match tracking_registry(metrics) {
        Ok(registry) => registry,
        Err(response) => return response,
    };
    let current = crate::http::inflight::registry().by_connection();
    match ConnectionListResponse::current(query, registry, &listener_facts(generations), current) {
        Ok(response) => serialized_response(
            200,
            &response,
            "connections_serialization_failed",
            "could not serialize the connection list",
        ),
        Err(message) => ApiResponse::error(400, "invalid_connection_query", message),
    }
}

pub(super) fn requests(method: &str, query: Option<&str>) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    match RequestListResponse::current(query) {
        Ok(response) => serialized_response(
            200,
            &response,
            "requests_serialization_failed",
            "could not serialize in-flight requests",
        ),
        Err(message) => ApiResponse::error(400, "invalid_request_query", message),
    }
}

fn status_response(
    metrics: &RuntimeMetrics,
    generations: Option<&GenerationManager>,
    topology: &TopologySnapshot,
    coordinator: Option<&CanonicalConfigCoordinator>,
) -> ApiResponse {
    let Ok(runtime) = metrics.snapshot() else {
        return ApiResponse::error(
            503,
            "status_unavailable",
            "runtime status could not be sampled",
        );
    };
    let generation = generations.map(GenerationManager::status);
    let audit = crate::observability::operational_event::audit_status();
    let response = match StatusResponse::project(
        runtime,
        generation.as_ref(),
        audit,
        topology,
        metrics.supervision_mode(),
        coordinator.is_some_and(|coordinator| coordinator.write_block().is_none()),
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
    let Ok(serving) = metrics.serving() else {
        return ApiResponse::error(503, "not_ready", "runtime metrics are unavailable");
    };
    let response = ReadinessResponse::project(serving, generations.map(GenerationManager::status));
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
    match serde_json::to_vec(response) {
        Ok(body) => ApiResponse::bytes(status, body, "application/json"),
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

fn stats_response(generations: Option<&GenerationManager>) -> ApiResponse {
    let Some(generations) = generations else {
        return ApiResponse::error(503, "stats_unavailable", "generation state is unavailable");
    };
    match crate::observability::stats_page::management_document(generations) {
        Ok(body) => ApiResponse::bytes(200, body, "application/json"),
        Err(error) => ApiResponse::error(503, "stats_unavailable", error.to_string()),
    }
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
    match response.body() {
        Ok(body) => ApiResponse::bytes(200, body, "application/json"),
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
    match TopologyResponse::active_body(topology, &runtime) {
        Ok(body) => ApiResponse::bytes(200, body, "application/json"),
        Err(error) => ApiResponse::error(
            500,
            "topology_serialization_failed",
            format!("could not serialize active runtime topology: {error}"),
        ),
    }
}
