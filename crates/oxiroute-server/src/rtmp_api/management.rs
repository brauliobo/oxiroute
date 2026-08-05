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
    operational_event::{AuditCategory, AuditContext, AuditResult, AuditStore},
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
    audit: Arc<AuditStore>,
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
    TlsRenew,
    TlsRevoke,
    TlsDelete,
    TlsAccountRollover,
    TlsJobCancel,
    TlsJobPause,
    TlsJobResume,
    Audit(Option<&'a str>),
    AuditStatus,
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
        "/api/v1/tls/renew" => Some(Route::TlsRenew),
        "/api/v1/tls/revoke" => Some(Route::TlsRevoke),
        "/api/v1/tls/delete" => Some(Route::TlsDelete),
        "/api/v1/tls/account/rollover" => Some(Route::TlsAccountRollover),
        "/api/v1/tls/jobs/cancel" => Some(Route::TlsJobCancel),
        "/api/v1/tls/jobs/pause" => Some(Route::TlsJobPause),
        "/api/v1/tls/jobs/resume" => Some(Route::TlsJobResume),
        "/api/v1/audit" => Some(Route::Audit(query)),
        "/api/v1/audit/status" => Some(Route::AuditStatus),
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
        audit: Arc<AuditStore>,
    ) -> Self {
        Self {
            coordinator,
            generations,
            metrics,
            audit,
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
        context: &AuditContext,
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
            (Route::TlsRenew, "POST") => self.tls_renew(session, context).await,
            (Route::TlsRevoke, "POST") => self.tls_revoke(session, context).await,
            (Route::TlsDelete, "POST") => self.tls_delete(session, context).await,
            (Route::TlsAccountRollover, "POST") => {
                self.tls_account_rollover(session, context).await
            }
            (Route::TlsJobCancel, "POST") => self.tls_job_control(session, context, JobControl::Cancel).await,
            (Route::TlsJobPause, "POST") => self.tls_job_control(session, context, JobControl::Pause).await,
            (Route::TlsJobResume, "POST") => self.tls_job_control(session, context, JobControl::Resume).await,
            (Route::Audit(query), "GET") => self.audit(query),
            (Route::AuditStatus, "GET") => self.audit_status(),
            (Route::Events(query), "GET") => Self::events(query),
            (Route::ProcessDrain, "POST") => self.process_drain(session).await,
            (Route::ProcessShutdown, "POST") => self.process_shutdown(session).await,
            (
                Route::Listeners
                | Route::Pools
                | Route::Servers
                | Route::Generations
                | Route::Tls
                | Route::TlsRenew
                | Route::Audit(_)
                | Route::AuditStatus
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
        let certificates =
            active
                .config()
                .certificates
                .iter()
                .map(|certificate| {
                    let (source, development_only, status) =
                        match &certificate.source {
                            CertificateSource::Files { .. } => (
                                "files",
                                false,
                                snapshot
                                    .direct_file_certificates
                                    .iter()
                                    .find(|status| status.name == certificate.name)
                                    .map(|status| json!(status)),
                            ),
                            CertificateSource::Certbot { .. } => (
                                "certbot",
                                false,
                                snapshot
                                    .certbot_certificates
                                    .iter()
                                    .find(|status| status.name == certificate.name)
                                    .map(|status| json!(status)),
                            ),
                            CertificateSource::AcmeManaged { .. } => (
                                "acme_managed",
                                false,
                                active.plan().tls.acme_reconcilers().iter().find_map(
                                    |reconciler| {
                                        (reconciler.status().certificate == certificate.name)
                                            .then(|| json!(reconciler.status()))
                                    },
                                ),
                            ),
                            CertificateSource::SelfSignedDevelopment { .. } => (
                                "self_signed_development",
                                true,
                                active.plan().tls.certificates().get(&certificate.name).map(
                                    |active| {
                                        let metadata = active.snapshot();
                                        json!({
                                            "activeContentRevision": metadata.metadata().revision,
                                            "expiresAt": metadata.metadata().validity.not_after,
                                        })
                                    },
                                ),
                            ),
                        };
                    json!({
                        "name": certificate.name,
                        "dnsNames": certificate.dns_names,
                        "source": source,
                        "developmentOnly": development_only,
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
        let managed_reconcilers = active.plan().tls.acme_reconcilers();
        if let Some(name) = request.certificate.as_deref() {
            if !reconcilers
                .iter()
                .any(|reconciler| reconciler.status().certificate == name)
                && !managed_reconcilers
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
        for reconciler in managed_reconcilers {
            let status = reconciler.status();
            if request
                .certificate
                .as_deref()
                .is_none_or(|name| name == status.certificate)
            {
                match reconciler.reconcile() {
                    Ok(outcome) => outcomes.push(json!({
                        "certificate": status.certificate,
                        "outcome": outcome.code(),
                        "diskRevision": reconciler.status().disk_revision,
                        "activeRevision": reconciler.status().active_revision,
                    })),
                    Err(_) => {
                        return ApiResponse::error(
                            503,
                            "managed_certificate_reconciliation_failed",
                            "managed certificate reconciliation failed",
                        );
                    }
                }
            }
        }
        ApiResponse::json(200, &json!({ "outcomes": outcomes }))
    }

    async fn tls_renew(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
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
        let reconciler = {
            let active = mutation.generation();
            active
                .plan()
                .tls
                .acme_reconcilers()
                .iter()
                .find(|reconciler| {
                    request
                        .certificate
                        .as_deref()
                        .is_none_or(|name| name == reconciler.status().certificate)
                })
                .cloned()
        };
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate_name = reconciler.status().certificate;
        crate::operational_event::emit_certificate_with_context(
            "certificate_renewal",
            "requested",
            &certificate_name,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.renew_now_with_correlation(correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok(outcome)) => {
                let status = reconciler.status();
                crate::operational_event::emit_certificate_with_context(
                    if outcome == crate::AcmeManagedOutcome::Activated {
                        "certificate_activated"
                    } else {
                        "certificate_renewal"
                    },
                    if outcome == crate::AcmeManagedOutcome::Activated {
                        "activated"
                    } else {
                        "applied"
                    },
                    &status.certificate,
                    context,
                );
                ApiResponse::json(
                    200,
                    &json!({
                        "certificate": status.certificate,
                        "outcome": outcome.code(),
                        "diskRevision": status.disk_revision,
                        "activeRevision": status.active_revision,
                    }),
                )
            }
            Ok(Err(error)) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_renewal",
                    "failed",
                    &certificate_name,
                    context,
                );
                ApiResponse::error(503, error.code(), "managed ACME renewal failed")
            }
            Err(_) => ApiResponse::error(503, "renewal_worker_failed", "renewal worker failed"),
        }
    }

    async fn tls_revoke(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: TlsRevokeRequest = match body(session).await {
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
        let reconciler = {
            let active = mutation.generation();
            active
                .plan()
                .tls
                .acme_reconcilers()
                .iter()
                .find(|reconciler| reconciler.status().certificate == request.certificate)
                .cloned()
        };
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = reconciler.status().certificate;
        crate::operational_event::emit_certificate_with_context(
            "certificate_revocation",
            "requested",
            &certificate,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let reason = request.reason;
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.revoke_now_with_correlation(reason, correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok((outcome, job_id))) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_revocation",
                    "applied",
                    &certificate,
                    context,
                );
                ApiResponse::json(
                    200,
                    &json!({
                        "certificate": certificate,
                        "outcome": outcome.code(),
                        "jobId": job_id,
                    }),
                )
            }
            Ok(Err(error)) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_revocation",
                    "failed",
                    &certificate,
                    context,
                );
                managed_error_response(error, "managed ACME revocation failed")
            }
            Err(_) => ApiResponse::error(503, "revocation_worker_failed", "revocation worker failed"),
        }
    }

    async fn tls_delete(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let Some(certificate) = request.certificate.as_deref() else {
            return ApiResponse::error(
                400,
                "certificate_required",
                "a managed certificate is required",
            );
        };
        let mutation = match self
            .generations
            .begin_mutation(&request.expected_active_revision)
        {
            Ok(mutation) => mutation,
            Err(error) => return mutation_error(&error),
        };
        let active = mutation.generation();
        if active
            .config()
            .tls_profiles
            .iter()
            .any(|profile| {
                profile.default_certificate == certificate
                    || profile.certificates.iter().any(|name| name == certificate)
            })
        {
            return ApiResponse::error(
                409,
                "certificate_in_use",
                "managed certificate is still referenced by an active TLS profile",
            );
        }
        let reconciler = active
            .plan()
            .tls
            .acme_reconcilers()
            .iter()
            .find(|reconciler| reconciler.status().certificate == certificate)
            .cloned();
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = certificate.to_owned();
        crate::operational_event::emit_certificate_with_context(
            "certificate_deletion",
            "requested",
            &certificate,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.delete_state_with_correlation(correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok((outcome, job_id))) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_deletion",
                    "applied",
                    &certificate,
                    context,
                );
                ApiResponse::json(
                    200,
                    &json!({
                        "certificate": certificate,
                        "outcome": outcome.code(),
                        "jobId": job_id,
                    }),
                )
            }
            Ok(Err(error)) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_deletion",
                    "failed",
                    &certificate,
                    context,
                );
                managed_error_response(error, "managed ACME state deletion failed")
            }
            Err(_) => ApiResponse::error(503, "deletion_worker_failed", "deletion worker failed"),
        }
    }

    async fn tls_account_rollover(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
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
        let reconciler = {
            let active = mutation.generation();
            active
                .plan()
                .tls
                .acme_reconcilers()
                .iter()
                .find(|reconciler| {
                    request
                        .certificate
                        .as_deref()
                        .is_none_or(|name| name == reconciler.status().certificate)
                })
                .cloned()
        };
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = reconciler.status().certificate;
        crate::operational_event::emit_certificate_with_context(
            "certificate_account_rollover",
            "requested",
            &certificate,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler
                .rollover_account_key_with_correlation(correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok((outcome, job_id))) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_account_rollover",
                    "applied",
                    &certificate,
                    context,
                );
                ApiResponse::json(
                    200,
                    &json!({
                        "certificate": certificate,
                        "outcome": outcome.code(),
                        "jobId": job_id,
                    }),
                )
            }
            Ok(Err(error)) => {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_account_rollover",
                    "failed",
                    &certificate,
                    context,
                );
                managed_error_response(error, "managed ACME account rollover failed")
            }
            Err(_) => ApiResponse::error(
                503,
                "account_rollover_worker_failed",
                "account rollover worker failed",
            ),
        }
    }

    async fn tls_job_control(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
        control: JobControl,
    ) -> ApiResponse {
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
        let reconciler = {
            let active = mutation.generation();
            active
                .plan()
                .tls
                .acme_reconcilers()
                .iter()
                .find(|reconciler| {
                    request
                        .certificate
                        .as_deref()
                        .is_none_or(|name| name == reconciler.status().certificate)
                })
                .cloned()
        };
        drop(mutation);
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = reconciler.status().certificate;
        let result = match control {
            JobControl::Cancel => reconciler.cancel_job().map(|job_id| {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "requested",
                    &certificate,
                    context,
                );
                json!({ "certificate": certificate, "outcome": "cancellation_requested", "jobId": job_id })
            }),
            JobControl::Pause => reconciler.pause().map(|job_id| {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "applied",
                    &certificate,
                    context,
                );
                json!({ "certificate": certificate, "outcome": "paused", "jobId": job_id })
            }),
            JobControl::Resume => reconciler.resume().map(|()| {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "applied",
                    &certificate,
                    context,
                );
                json!({ "certificate": certificate, "outcome": "resumed" })
            }),
        };
        match result {
            Ok(response) => ApiResponse::json(202, &response),
            Err(error) => managed_error_response(error, "managed ACME job control failed"),
        }
    }

    fn audit_status(&self) -> ApiResponse {
        ApiResponse::json(200, &json!({ "audit": self.audit.status() }))
    }

    fn audit(&self, query: Option<&str>) -> ApiResponse {
        let mut after = 0_u64;
        let mut limit = 100_usize;
        let mut category = None;
        let mut result = None;
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
                                "audit cursor is invalid",
                            );
                        }
                    },
                    "limit" => match value.parse::<usize>() {
                        Ok(value) if (1..=MAX_EVENT_LIMIT).contains(&value) => limit = value,
                        _ => {
                            return ApiResponse::error(
                                400,
                                "invalid_limit",
                                "audit limit must be between 1 and 1000",
                            );
                        }
                    },
                    "category" => match AuditCategory::parse(value) {
                        Some(value) => category = Some(value),
                        None => {
                            return ApiResponse::error(
                                400,
                                "invalid_category",
                                "audit category is invalid",
                            );
                        }
                    },
                    "result" => match AuditResult::parse(value) {
                        Some(value) => result = Some(value),
                        None => {
                            return ApiResponse::error(
                                400,
                                "invalid_result",
                                "audit result is invalid",
                            );
                        }
                    },
                    _ => {
                        return ApiResponse::error(
                            400,
                            "invalid_query",
                            "audit query parameter is invalid",
                        );
                    }
                }
            }
        }
        let page = self.audit.page(after, limit, category, result);
        ApiResponse::json(
            200,
            &json!({
                "records": page.records,
                "cursor": page.cursor,
                "hasMore": page.has_more,
                "oldestCursor": page.oldest_cursor,
                "latestCursor": page.latest_cursor,
            }),
        )
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
    use crate::RuntimeReferenceKind::{
        ForwardHttp1, ForwardHttp3, Http1, Http2, Http3, Rtmp, Tcp, Udp, WebSocket,
    };
    [
        ForwardHttp1,
        ForwardHttp3,
        Http1,
        Http2,
        Http3,
        WebSocket,
        Tcp,
        Rtmp,
        Udp,
    ]
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

#[derive(Clone, Copy)]
enum JobControl {
    Cancel,
    Pause,
    Resume,
}

#[allow(clippy::needless_pass_by_value)]
fn managed_error_response(error: crate::AcmeManagedError, message: &str) -> ApiResponse {
    let status = match &error {
        crate::AcmeManagedError::Protocol(
            oxiroute_acme::AcmeError::InvalidRevocationReason,
        ) => 400,
        crate::AcmeManagedError::Busy
        | crate::AcmeManagedError::Paused
        | crate::AcmeManagedError::NoJob => 409,
        _ => 503,
    };
    ApiResponse::error(status, error.code(), message)
}

async fn body<T: for<'de> Deserialize<'de>>(session: &mut ServerSession) -> Result<T, ApiResponse> {
    let bytes = read_config_body(session).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiResponse::error(400, "invalid_json", "request body is invalid JSON"))
}

#[cfg(test)]
mod tests {
    use super::{Route, match_route};

    #[test]
    fn audit_routes_preserve_query_filters_and_status_is_distinct() {
        assert!(matches!(
            match_route("/api/v1/audit?after=4&limit=2&category=reload"),
            Some(Route::Audit(Some("after=4&limit=2&category=reload")))
        ));
        assert!(matches!(
            match_route("/api/v1/audit/status"),
            Some(Route::AuditStatus)
        ));
    }

    #[test]
    fn managed_acme_lifecycle_routes_are_exact_and_post_only() {
        for path in [
            "/api/v1/tls/revoke",
            "/api/v1/tls/delete",
            "/api/v1/tls/account/rollover",
            "/api/v1/tls/jobs/cancel",
            "/api/v1/tls/jobs/pause",
            "/api/v1/tls/jobs/resume",
        ] {
            assert!(match_route(path).is_some(), "route missing: {path}");
            assert!(match_route(&format!("{path}/")).is_none());
        }
    }

    #[test]
    fn invalid_revocation_reason_is_a_client_error() {
        let response = super::managed_error_response(
            crate::AcmeManagedError::Protocol(
                oxiroute_acme::AcmeError::InvalidRevocationReason,
            ),
            "revocation failed",
        );
        assert_eq!(response.status, 400);
    }
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TlsRevokeRequest {
    expected_active_revision: String,
    certificate: String,
    #[serde(default)]
    reason: Option<u8>,
}
