use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use oxiroute_config::CertificateSource;
use pingora::protocols::http::ServerSession;
use serde::Deserialize;
use serde_json::json;

use super::{ApiResponse, config::read_config_body};
use crate::{
    AdministrativeState, GenerationManager, HealthOverride, RuntimeGeneration, RuntimeMetrics,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};

const MAX_BATCH_TARGETS: usize = 256;
const MAX_EVENT_LIMIT: usize = 1_000;

type SelectedServers = (
    crate::GenerationMutation,
    Vec<(Arc<crate::RoundRobinPool>, String)>,
);

pub(super) struct ManagementState {
    coordinator: CanonicalConfigCoordinator,
    generations: GenerationManager,
    metrics: RuntimeMetrics,
    process_shutdown: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Listeners,
    ListenerState,
    Pools,
    PoolState,
    Servers,
    ServerState,
    ServerHealth,
    ServerChecks,
    ServerMaxConnections,
    ServerRefreshDns,
    Generations,
    GenerationReload,
    GenerationRollback,
    GenerationDrain,
    Tls,
    TlsReconcile,
    Events(Option<&'a str>),
    EventStream(Option<&'a str>),
    ProcessDrain,
    ProcessShutdown,
}

pub(super) fn match_route(path_and_query: &str) -> Option<Route<'_>> {
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    match path {
        "/api/v1/listeners" => Some(Route::Listeners),
        "/api/v1/listeners/administrative-state" => Some(Route::ListenerState),
        "/api/v1/pools" => Some(Route::Pools),
        "/api/v1/pools/administrative-state" => Some(Route::PoolState),
        "/api/v1/servers" => Some(Route::Servers),
        "/api/v1/servers/administrative-state" => Some(Route::ServerState),
        "/api/v1/servers/health-override" => Some(Route::ServerHealth),
        "/api/v1/servers/checks" => Some(Route::ServerChecks),
        "/api/v1/servers/max-connections" => Some(Route::ServerMaxConnections),
        "/api/v1/servers/refresh-dns" => Some(Route::ServerRefreshDns),
        "/api/v1/generations" => Some(Route::Generations),
        "/api/v1/generations/reload" => Some(Route::GenerationReload),
        "/api/v1/generations/rollback" => Some(Route::GenerationRollback),
        "/api/v1/generations/drain" => Some(Route::GenerationDrain),
        "/api/v1/tls" => Some(Route::Tls),
        "/api/v1/tls/reconcile" => Some(Route::TlsReconcile),
        "/api/v1/events" => Some(Route::Events(query)),
        "/api/v1/events/stream" => Some(Route::EventStream(query)),
        "/api/v1/process/drain" => Some(Route::ProcessDrain),
        "/api/v1/process/shutdown" => Some(Route::ProcessShutdown),
        _ => None,
    }
}

pub(super) struct EventStreamRoute<'a> {
    pub(super) query: Option<&'a str>,
}

pub(super) fn event_stream_route(
    path_and_query: &str,
    accepts_event_stream: bool,
) -> Option<EventStreamRoute<'_>> {
    match match_route(path_and_query)? {
        Route::Events(query) if accepts_event_stream => Some(EventStreamRoute { query }),
        Route::EventStream(query) => Some(EventStreamRoute { query }),
        _ => None,
    }
}

impl ManagementState {
    pub(super) fn new(
        coordinator: CanonicalConfigCoordinator,
        generations: GenerationManager,
        metrics: RuntimeMetrics,
    ) -> Self {
        Self {
            coordinator,
            generations,
            metrics,
            process_shutdown: None,
        }
    }

    pub(super) fn set_process_shutdown(&mut self, shutdown: Arc<AtomicBool>) {
        self.process_shutdown = Some(shutdown);
    }

    pub(super) async fn handle(
        &self,
        route: Route<'_>,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        match (route, method) {
            (Route::Listeners, "GET") => self.listeners(),
            (Route::ListenerState, "POST") => self.listener_state(session).await,
            (Route::Pools, "GET") => self.pools(),
            (Route::PoolState, "POST") => self.pool_state(session).await,
            (Route::Servers, "GET") => self.servers(),
            (Route::ServerState, "POST") => self.server_state(session).await,
            (Route::ServerHealth, "POST") => self.server_health(session).await,
            (Route::ServerChecks, "POST") => self.server_checks(session).await,
            (Route::ServerMaxConnections, "PUT") => self.server_max_connections(session).await,
            (Route::ServerRefreshDns, "POST") => self.server_refresh_dns(session).await,
            (Route::Generations, "GET") => {
                ApiResponse::json(200, &json!({ "generation": self.generations.status() }))
            }
            (Route::GenerationReload, "POST") => self.generation_reload(session).await,
            (Route::GenerationRollback, "POST") => self.generation_rollback(session).await,
            (Route::GenerationDrain, "POST") => self.generation_drain(session).await,
            (Route::Tls, "GET") => self.tls(),
            (Route::TlsReconcile, "POST") => self.tls_reconcile(session).await,
            (Route::Events(query), "GET") => Self::events(query),
            (Route::ProcessDrain, "POST") => self.process_drain(session).await,
            (Route::ProcessShutdown, "POST") => self.process_shutdown(session).await,
            (
                Route::Listeners
                | Route::Pools
                | Route::Servers
                | Route::Generations
                | Route::Tls
                | Route::Events(_)
                | Route::EventStream(_),
                _,
            ) => ApiResponse::method_not_allowed("GET"),
            (Route::ServerMaxConnections, _) => ApiResponse::method_not_allowed("PUT"),
            _ => ApiResponse::method_not_allowed("POST"),
        }
    }

    fn active(&self) -> Result<Arc<RuntimeGeneration>, ApiResponse> {
        self.generations.active().ok_or_else(|| {
            ApiResponse::error(
                503,
                "generation_unavailable",
                "no active generation is available",
            )
        })
    }

    fn listeners(&self) -> ApiResponse {
        match self.metrics.snapshot() {
            Ok(snapshot) => ApiResponse::json(200, &json!({ "listeners": snapshot.listeners })),
            Err(_) => ApiResponse::error(
                503,
                "listeners_unavailable",
                "listener state is unavailable",
            ),
        }
    }

    fn pools(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let pools = active
            .plan()
            .pools
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect::<Vec<_>>();
        ApiResponse::json(200, &json!({ "pools": pools }))
    }

    fn servers(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let servers = active
            .plan()
            .pools
            .iter()
            .flat_map(|pool| {
                let snapshot = pool.health_snapshot();
                snapshot.endpoints.into_iter().map(move |server| {
                    json!({
                        "pool": snapshot.name,
                        "server": server,
                    })
                })
            })
            .collect::<Vec<_>>();
        ApiResponse::json(200, &json!({ "servers": servers }))
    }

    async fn listener_state(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ListenerStateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        if request.listeners.is_empty() || request.listeners.len() > MAX_BATCH_TARGETS {
            return invalid_batch();
        }
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        let metrics = mutation.generation().metrics();
        let Ok(snapshot) = metrics.snapshot() else {
            return ApiResponse::error(
                503,
                "listeners_unavailable",
                "listener state is unavailable",
            );
        };
        if request.listeners.iter().any(|name| {
            !snapshot
                .listeners
                .iter()
                .any(|listener| &listener.name == name)
        }) {
            return ApiResponse::error(404, "listener_not_found", "a listener was not found");
        }
        for name in &request.listeners {
            if metrics
                .set_listener_administrative_state(name, request.state)
                .is_err()
            {
                return ApiResponse::error(
                    500,
                    "listener_update_failed",
                    "listener state could not be changed",
                );
            }
        }
        mutation_response("listener_administrative_state", request.listeners.len())
    }

    async fn pool_state(&self, session: &mut ServerSession) -> ApiResponse {
        let request: PoolStateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        let active = mutation.generation();
        if request.pools.is_empty() || request.pools.len() > MAX_BATCH_TARGETS {
            return invalid_batch();
        }
        let mut selected = Vec::with_capacity(request.pools.len());
        for name in &request.pools {
            let Some(pool) = find_pool(active, name) else {
                return ApiResponse::error(404, "pool_not_found", "an upstream pool was not found");
            };
            selected.push(pool);
        }
        for pool in selected {
            pool.set_administrative_state(request.state);
        }
        mutation_response("pool_administrative_state", request.pools.len())
    }

    async fn server_state(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerStateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_administrative_state(server, request.state)
        })
    }

    async fn server_health(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerHealthRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_health_override(server, request.health)
        })
    }

    async fn server_checks(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerChecksRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_checks_enabled(server, request.enabled)
        })
    }

    async fn server_max_connections(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerCapacityRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_max_connections(server, request.max_connections)
        })
    }

    async fn server_refresh_dns(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerChange = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let (mutation, selected) = match self.select_servers(&request) {
            Ok(selected) => selected,
            Err(response) => return response,
        };
        drop(mutation);
        let mut refreshed = Vec::with_capacity(selected.len());
        let mut failed = false;
        let mut resolutions = Vec::with_capacity(selected.len());
        for (pool, server) in &selected {
            let pool_name = pool.health_snapshot().name;
            if let Ok(addresses) = pool.resolve_server_dns(server).await {
                refreshed.push(json!({
                    "pool": pool_name,
                    "server": server,
                    "outcome": "refreshed",
                    "addresses": &addresses,
                }));
                resolutions.push(Some(addresses));
            } else {
                failed = true;
                resolutions.push(None);
                refreshed.push(json!({
                    "pool": pool_name,
                    "server": server,
                    "outcome": "failed",
                    "error": { "code": "dns_refresh_failed" },
                }));
            }
        }
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        for (target, addresses) in request.targets.iter().zip(resolutions) {
            let Some(addresses) = addresses else {
                continue;
            };
            let Some(pool) = find_pool(mutation.generation(), &target.pool) else {
                return ApiResponse::error(
                    409,
                    "generation_conflict",
                    "the active generation revision changed",
                );
            };
            if pool.commit_server_dns(&target.server, &addresses).is_err() {
                return ApiResponse::error(
                    500,
                    "dns_refresh_failed",
                    "DNS addresses could not be committed",
                );
            }
        }
        ApiResponse::json(
            if failed { 207 } else { 200 },
            &json!({
                "outcome": if failed { "partially_refreshed" } else { "refreshed" },
                "atomic": false,
                "servers": refreshed,
            }),
        )
    }

    fn apply_servers(
        &self,
        change: &ServerChange,
        apply: impl Fn(&Arc<crate::RoundRobinPool>, &str) -> Result<(), crate::PoolAdminError>,
    ) -> ApiResponse {
        let (_mutation, selected) = match self.select_servers(change) {
            Ok(selected) => selected,
            Err(response) => return response,
        };
        for (pool, server) in &selected {
            if apply(pool, server).is_err() {
                return ApiResponse::error(
                    500,
                    "server_update_failed",
                    "server state could not be changed",
                );
            }
        }
        mutation_response("server_update", selected.len())
    }

    fn select_servers(&self, change: &ServerChange) -> Result<SelectedServers, ApiResponse> {
        let mutation = self
            .generations
            .begin_mutation(&change.expected_active_revision)
            .map_err(|error| mutation_error(&error))?;
        let active = mutation.generation();
        if change.targets.is_empty() || change.targets.len() > MAX_BATCH_TARGETS {
            return Err(invalid_batch());
        }
        let mut selected = Vec::with_capacity(change.targets.len());
        for target in &change.targets {
            let pool = find_pool(active, &target.pool).ok_or_else(|| {
                ApiResponse::error(404, "pool_not_found", "an upstream pool was not found")
            })?;
            if !pool.has_server(&target.server) {
                return Err(ApiResponse::error(
                    404,
                    "server_not_found",
                    "an upstream server was not found",
                ));
            }
            selected.push((pool, target.server.clone()));
        }
        Ok((mutation, selected))
    }

    async fn generation_reload(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let _mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        let document = match self.coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document,
            ConfigLoadOutcome::Rejected(rejection) => {
                return ApiResponse::json(
                    422,
                    &json!({
                        "error": {
                            "code": "config_rejected",
                            "message": "persisted configuration is invalid",
                        },
                        "diskRevision": rejection.disk_revision,
                        "activeRevision": self.generations.status().active_revision,
                        "diagnostics": rejection.diagnostics,
                    }),
                );
            }
        };
        match self.generations.prepare(*document) {
            Ok(candidate) => ApiResponse::json(
                202,
                &json!({
                    "outcome": "startup_requested",
                    "candidateRevision": candidate.revision().candidate,
                }),
            ),
            Err(error) => ApiResponse::error(
                422,
                error.code(),
                "configuration generation could not be prepared",
            ),
        }
    }

    async fn generation_rollback(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let _mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        match self.generations.rollback() {
            Ok(candidate) => ApiResponse::json(
                202,
                &json!({
                    "outcome": "rollback_startup_requested",
                    "candidateRevision": candidate.revision().candidate,
                }),
            ),
            Err(error) => {
                ApiResponse::error(409, error.code(), "no previous generation can be activated")
            }
        }
    }

    async fn generation_drain(&self, session: &mut ServerSession) -> ApiResponse {
        let request: DrainRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        let active = mutation.generation();
        let timeout_ms = request.timeout_ms.unwrap_or(0);
        if timeout_ms > 300_000 {
            return ApiResponse::error(
                400,
                "invalid_timeout",
                "drain timeout must not exceed 300000 milliseconds",
            );
        }
        active.stop_accepting();
        active
            .metrics()
            .set_process_administrative_state(AdministrativeState::Drain);
        ApiResponse::json(
            202,
            &json!({
                "outcome": if active.drained() { "drained" } else { "draining" },
                "activeReferences": active_reference_count(active),
            }),
        )
    }

    async fn process_drain(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        mutation
            .generation()
            .metrics()
            .set_process_administrative_state(AdministrativeState::Drain);
        ApiResponse::json(202, &json!({ "outcome": "draining" }))
    }

    async fn process_shutdown(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let Some(shutdown) = &self.process_shutdown else {
            return ApiResponse::error(
                503,
                "shutdown_unavailable",
                "process shutdown control is unavailable",
            );
        };
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        drop(
            self.generations
                .begin_shutdown(Instant::now() + Duration::from_secs(5)),
        );
        mutation
            .generation()
            .metrics()
            .set_process_administrative_state(AdministrativeState::Drain);
        shutdown.store(true, Ordering::Release);
        crate::operational_event::emit("process_shutdown", "requested", None);
        ApiResponse::json(202, &json!({ "outcome": "shutdown_requested" }))
    }

    fn tls(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let Ok(snapshot) = active.metrics().snapshot() else {
            return ApiResponse::error(503, "tls_status_unavailable", "TLS status is unavailable");
        };
        let certificates = active
            .config()
            .certificates
            .iter()
            .map(|certificate| {
                let source = match &certificate.source {
                    CertificateSource::Files { .. } => "files",
                    CertificateSource::Certbot { .. } => "certbot",
                };
                let status = snapshot
                    .certbot_certificates
                    .iter()
                    .find(|status| status.name == certificate.name);
                json!({
                    "name": certificate.name,
                    "dnsNames": certificate.dns_names,
                    "source": source,
                    "status": status,
                })
            })
            .collect::<Vec<_>>();
        ApiResponse::json(
            200,
            &json!({
                "certificates": certificates,
                "watcher": snapshot.certbot_watcher,
            }),
        )
    }

    async fn tls_reconcile(&self, session: &mut ServerSession) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        let active = mutation.generation();
        let reconcilers = active.plan().certbot_reconcilers();
        if let Some(name) = request.certificate.as_deref() {
            if !reconcilers
                .iter()
                .any(|reconciler| reconciler.status().certificate == name)
            {
                return ApiResponse::error(
                    404,
                    "certificate_not_found",
                    "certificate was not found",
                );
            }
        }
        let mut outcomes = Vec::new();
        for reconciler in reconcilers {
            let status = reconciler.status();
            if request
                .certificate
                .as_deref()
                .is_none_or(|name| name == status.certificate)
            {
                match reconciler.reconcile() {
                    Ok(outcome) => {
                        let (previous, archive) = match outcome {
                            crate::CertbotReconcileOutcome::Unchanged { archive_revision } => {
                                (None, archive_revision)
                            }
                            crate::CertbotReconcileOutcome::Activated {
                                previous_archive_revision,
                                archive_revision,
                                ..
                            } => (Some(previous_archive_revision), archive_revision),
                        };
                        outcomes.push(json!({
                            "certificate": status.certificate,
                            "outcome": outcome.code(),
                            "previousArchiveRevision": previous.map(|value| value.to_string()),
                            "archiveRevision": archive.to_string(),
                        }));
                    }
                    Err(error) => {
                        return ApiResponse::error(
                            503,
                            error.code(),
                            "certificate reconciliation failed",
                        );
                    }
                }
            }
        }
        ApiResponse::json(200, &json!({ "outcomes": outcomes }))
    }

    fn events(query: Option<&str>) -> ApiResponse {
        let mut after = 0_u64;
        let mut limit = 100_usize;
        if let Some(query) = query {
            for pair in query.split('&') {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                match name {
                    "after" => match value.parse() {
                        Ok(value) => after = value,
                        Err(_) => {
                            return ApiResponse::error(
                                400,
                                "invalid_cursor",
                                "event cursor is invalid",
                            );
                        }
                    },
                    "limit" => match value.parse::<usize>() {
                        Ok(value) if (1..=MAX_EVENT_LIMIT).contains(&value) => limit = value,
                        _ => {
                            return ApiResponse::error(
                                400,
                                "invalid_limit",
                                "event limit must be between 1 and 1000",
                            );
                        }
                    },
                    _ => {
                        return ApiResponse::error(
                            400,
                            "invalid_query",
                            "event query parameter is invalid",
                        );
                    }
                }
            }
        }
        let (events, cursor, has_more, oldest_cursor) =
            crate::operational_event::list(after, limit);
        ApiResponse::json(
            200,
            &json!({
                "events": events,
                "cursor": cursor,
                "hasMore": has_more,
                "oldestCursor": oldest_cursor,
            }),
        )
    }
}

fn find_pool(active: &RuntimeGeneration, name: &str) -> Option<Arc<crate::RoundRobinPool>> {
    active
        .plan()
        .pools
        .iter()
        .find(|pool| pool.health_snapshot().name == name)
        .cloned()
}

fn active_reference_count(active: &RuntimeGeneration) -> u64 {
    use crate::RuntimeReferenceKind::{ForwardHttp1, Http1, Http2, Rtmp, Tcp, WebSocket};
    [ForwardHttp1, Http1, Http2, WebSocket, Tcp, Rtmp]
        .into_iter()
        .map(|kind| active.active_references(kind))
        .sum()
}

fn mutation_response(operation: &str, changed: usize) -> ApiResponse {
    crate::operational_event::emit(operation, "applied", None);
    ApiResponse::json(200, &json!({ "outcome": "applied", "changed": changed }))
}

fn invalid_batch() -> ApiResponse {
    ApiResponse::error(
        400,
        "invalid_batch",
        "batch must contain between 1 and 256 targets",
    )
}

fn mutation_error(error: &crate::GenerationError) -> ApiResponse {
    let status = match error {
        crate::GenerationError::NoActive => 503,
        _ => 409,
    };
    ApiResponse::error(
        status,
        error.code(),
        "the active generation could not be mutated",
    )
}

async fn body<T: for<'de> Deserialize<'de>>(session: &mut ServerSession) -> Result<T, ApiResponse> {
    let bytes = read_config_body(session).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiResponse::error(400, "invalid_json", "request body is invalid JSON"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListenerStateRequest {
    listeners: Vec<String>,
    state: AdministrativeState,
    expected_active_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PoolStateRequest {
    pools: Vec<String>,
    state: AdministrativeState,
    expected_active_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerTarget {
    pool: String,
    server: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerChange {
    targets: Vec<ServerTarget>,
    expected_active_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerStateRequest {
    #[serde(flatten)]
    change: ServerChange,
    state: AdministrativeState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerHealthRequest {
    #[serde(flatten)]
    change: ServerChange,
    health: HealthOverride,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerChecksRequest {
    #[serde(flatten)]
    change: ServerChange,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerCapacityRequest {
    #[serde(flatten)]
    change: ServerChange,
    max_connections: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionRequest {
    expected_active_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DrainRequest {
    expected_active_revision: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TlsRequest {
    expected_active_revision: String,
    #[serde(default)]
    certificate: Option<String>,
}
