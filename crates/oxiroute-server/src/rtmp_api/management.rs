use std::{sync::Arc, time::Duration};

use oxiroute_config::CertificateSource;
use pingora::protocols::http::ServerSession;
use serde::{Deserialize, Serialize};

use super::{
    ApiResponse,
    config::read_config_body,
    dto::{
        AuditPageResponse, AuditStatusResponse, ConfigRejectedResponse, DnsRefreshResponse,
        DnsRefreshServer, DrainRequest, DrainResponse, EventPageV1Response, EventPageV2Response,
        GenerationActionResponse, GenerationResponse, ListenerInventoryResponse,
        ListenerStateRequest, MutationResponse, PoolInventoryResponse, PoolStateRequest,
        ProcessMutationResponse, RevisionRequest, ServerCapacityRequest, ServerChange,
        ServerChecksRequest, ServerHealthRequest, ServerInventoryResponse, ServerStateRequest,
        TlsActionResponse, TlsCertificateDto, TlsInventoryResponse, TlsJobControlResponse,
        TlsReconcileOutcome, TlsReconcileResponse, TlsRenewResponse, TlsRequest, TlsRevokeRequest,
    },
};
use crate::{
    AdministrativeState, GenerationManager, RuntimeGeneration, RuntimeMetrics,
    lifecycle_control::{LifecycleError, LifecycleOutcome, LifecyclePort},
    operational_event::{AuditCategory, AuditContext, AuditResult, AuditStore},
};

const MAX_BATCH_TARGETS: usize = 256;
const MAX_EVENT_LIMIT: usize = 1_000;

type SelectedServers = (
    crate::GenerationMutation,
    Vec<(Arc<crate::RoundRobinPool>, String)>,
);

pub(super) struct ManagementState {
    generations: GenerationManager,
    lifecycle: Arc<LifecyclePort>,
    metrics: RuntimeMetrics,
    audit: Arc<AuditStore>,
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
    Events(EventApiVersion, Option<&'a str>),
    EventStream(EventApiVersion, Option<&'a str>),
    ProcessDrain,
    ProcessShutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EventApiVersion {
    V1,
    V2,
}

pub(super) fn match_route(path_and_query: &str) -> Option<Route<'_>> {
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    match path {
        "/api/v1/listeners/administrative-state" => Some(Route::ListenerState),
        "/api/v1/pools/administrative-state" => Some(Route::PoolState),
        "/api/v1/servers/administrative-state" => Some(Route::ServerState),
        "/api/v1/servers/health-override" => Some(Route::ServerHealth),
        "/api/v1/servers/checks" => Some(Route::ServerChecks),
        "/api/v1/servers/max-connections" => Some(Route::ServerMaxConnections),
        "/api/v1/servers/refresh-dns" => Some(Route::ServerRefreshDns),
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
        "/api/v1/events" => Some(Route::Events(EventApiVersion::V1, query)),
        "/api/v1/events/stream" => Some(Route::EventStream(EventApiVersion::V1, query)),
        "/api/v2/events" => Some(Route::Events(EventApiVersion::V2, query)),
        "/api/v2/events/stream" => Some(Route::EventStream(EventApiVersion::V2, query)),
        "/api/v1/process/drain" => Some(Route::ProcessDrain),
        "/api/v1/process/shutdown" => Some(Route::ProcessShutdown),
        _ => None,
    }
}

pub(super) struct EventStreamRoute<'a> {
    pub(super) version: EventApiVersion,
    pub(super) query: Option<&'a str>,
}

pub(super) fn event_stream_route(
    path_and_query: &str,
    accepts_event_stream: bool,
) -> Option<EventStreamRoute<'_>> {
    match match_route(path_and_query)? {
        Route::Events(version, query) if accepts_event_stream => {
            Some(EventStreamRoute { version, query })
        }
        Route::EventStream(version, query) => Some(EventStreamRoute { version, query }),
        _ => None,
    }
}

impl ManagementState {
    pub(super) fn new(
        generations: GenerationManager,
        lifecycle: Arc<LifecyclePort>,
        metrics: RuntimeMetrics,
        audit: Arc<AuditStore>,
    ) -> Self {
        Self {
            generations,
            lifecycle,
            metrics,
            audit,
        }
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
            (Route::Generations, "GET") => ApiResponse::json(
                200,
                &serde_json::to_value(GenerationResponse::from(self.lifecycle.status()))
                    .expect("generation response DTO serializes"),
            ),
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
            (Route::TlsJobCancel, "POST") => {
                self.tls_job_control(session, context, JobControl::Cancel)
                    .await
            }
            (Route::TlsJobPause, "POST") => {
                self.tls_job_control(session, context, JobControl::Pause)
                    .await
            }
            (Route::TlsJobResume, "POST") => {
                self.tls_job_control(session, context, JobControl::Resume)
                    .await
            }
            (Route::Audit(query), "GET") => self.audit(query),
            (Route::AuditStatus, "GET") => self.audit_status(),
            (Route::Events(version, query), "GET") => Self::events(version, query),
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
                | Route::Events(_, _)
                | Route::EventStream(_, _),
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
            Ok(snapshot) => ApiResponse::json(
                200,
                &serde_json::to_value(ListenerInventoryResponse::from(snapshot))
                    .expect("listener inventory DTO serializes"),
            ),
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
            .pools()
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect::<Vec<_>>();
        ApiResponse::json(
            200,
            &serde_json::to_value(PoolInventoryResponse::from(pools))
                .expect("pool inventory DTO serializes"),
        )
    }

    fn servers(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let pools = active
            .pools()
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect::<Vec<_>>();
        ApiResponse::json(
            200,
            &serde_json::to_value(ServerInventoryResponse::from(pools))
                .expect("server inventory DTO serializes"),
        )
    }

    async fn listener_state(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ListenerStateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        if request.listeners.is_empty() || request.listeners.len() > MAX_BATCH_TARGETS {
            return invalid_batch();
        }
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
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
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
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
                refreshed.push(DnsRefreshServer::refreshed(
                    pool_name,
                    server.clone(),
                    &addresses,
                ));
                resolutions.push(Some(addresses));
            } else {
                failed = true;
                resolutions.push(None);
                refreshed.push(DnsRefreshServer::failed(pool_name, server.clone()));
            }
        }
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
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
        dto_response(
            if failed { 207 } else { 200 },
            &DnsRefreshResponse::new(failed, refreshed),
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
        let mutation = begin_mutation(&self.generations, &change.expected_active_revision)?;
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
        match self.lifecycle.execute(
            self.lifecycle
                .request_reload(&request.expected_active_revision),
            None,
        ) {
            Ok(LifecycleOutcome::Prepared(candidate_revision)) => {
                dto_response(202, &GenerationActionResponse::startup(&candidate_revision))
            }
            Err(LifecycleError::ConfigRejected {
                rejection,
                active_revision,
            }) => dto_response(
                422,
                &ConfigRejectedResponse::new(
                    rejection.disk_revision,
                    active_revision,
                    rejection.diagnostics,
                ),
            ),
            Err(LifecycleError::Preparation(error)) => ApiResponse::error(
                422,
                error.code(),
                "configuration generation could not be prepared",
            ),
            Err(LifecycleError::Mutation(error)) => mutation_error(&error),
            Ok(_) | Err(_) => unreachable!("reload lifecycle outcome"),
        }
    }

    async fn generation_rollback(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        match self.lifecycle.execute(
            self.lifecycle
                .request_rollback(&request.expected_active_revision),
            None,
        ) {
            Ok(LifecycleOutcome::Prepared(candidate_revision)) => dto_response(
                202,
                &GenerationActionResponse::rollback(&candidate_revision),
            ),
            Err(LifecycleError::Rollback(error)) => {
                ApiResponse::error(409, error.code(), "no previous generation can be activated")
            }
            Err(LifecycleError::Mutation(error)) => mutation_error(&error),
            Ok(_) | Err(_) => unreachable!("rollback lifecycle outcome"),
        }
    }

    async fn generation_drain(&self, session: &mut ServerSession) -> ApiResponse {
        let request: DrainRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let timeout_ms = request.timeout_ms.unwrap_or(0);
        match self.lifecycle.execute(
            self.lifecycle
                .request_drain(&request.expected_active_revision),
            Some(Duration::from_millis(timeout_ms)),
        ) {
            Ok(LifecycleOutcome::Drained {
                drained,
                active_references,
            }) => dto_response(202, &DrainResponse::new(drained, active_references)),
            Err(LifecycleError::InvalidDrainTimeout) => ApiResponse::error(
                400,
                "invalid_timeout",
                "drain timeout must not exceed 300000 milliseconds",
            ),
            Err(LifecycleError::Mutation(error)) => mutation_error(&error),
            Ok(_) | Err(_) => unreachable!("drain lifecycle outcome"),
        }
    }

    async fn process_drain(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        mutation
            .generation()
            .metrics()
            .set_process_administrative_state(AdministrativeState::Drain);
        dto_response(202, &ProcessMutationResponse::draining())
    }

    async fn process_shutdown(&self, session: &mut ServerSession) -> ApiResponse {
        let request: RevisionRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        match self.lifecycle.execute(
            self.lifecycle
                .request_shutdown(&request.expected_active_revision),
            None,
        ) {
            Ok(LifecycleOutcome::ShutdownRequested) => {
                dto_response(202, &ProcessMutationResponse::shutdown_requested())
            }
            Err(LifecycleError::ShutdownUnavailable) => ApiResponse::error(
                503,
                "shutdown_unavailable",
                "process shutdown control is unavailable",
            ),
            Err(LifecycleError::Mutation(error)) => mutation_error(&error),
            Ok(_) | Err(_) => unreachable!("shutdown lifecycle outcome"),
        }
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
            .as_draft()
            .certificates
            .iter()
            .map(|certificate| match &certificate.source {
                CertificateSource::Files { .. } => TlsCertificateDto::files(
                    certificate.name.clone(),
                    certificate.dns_names.clone(),
                    snapshot
                        .direct_file_certificates
                        .iter()
                        .find(|status| status.name == certificate.name)
                        .cloned(),
                ),
                CertificateSource::Certbot { .. } => TlsCertificateDto::certbot(
                    certificate.name.clone(),
                    certificate.dns_names.clone(),
                    snapshot
                        .certbot_certificates
                        .iter()
                        .find(|status| status.name == certificate.name)
                        .cloned(),
                ),
                CertificateSource::AcmeManaged { .. } => TlsCertificateDto::managed(
                    certificate.name.clone(),
                    certificate.dns_names.clone(),
                    active
                        .tls()
                        .acme_reconcilers()
                        .iter()
                        .find_map(|reconciler| {
                            (reconciler.status().certificate == certificate.name)
                                .then(|| reconciler.status())
                        }),
                ),
                CertificateSource::SelfSignedDevelopment { .. } => {
                    let status = active
                        .tls()
                        .certificates()
                        .get(&certificate.name)
                        .map(|active| {
                            let metadata = active.snapshot();
                            (
                                metadata.metadata().revision.clone(),
                                metadata.metadata().validity.not_after.clone(),
                            )
                        });
                    let (revision, expires_at) = status.unzip();
                    TlsCertificateDto::self_signed(
                        certificate.name.clone(),
                        certificate.dns_names.clone(),
                        revision,
                        expires_at,
                    )
                }
            })
            .collect::<Vec<_>>();
        dto_response(
            200,
            &TlsInventoryResponse::new(certificates, snapshot.certbot_watcher),
        )
    }

    async fn tls_reconcile(&self, session: &mut ServerSession) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let active = mutation.generation();
        let reconcilers = active.tls().certbot_reconcilers();
        let managed_reconcilers = active.tls().acme_reconcilers();
        if let Some(name) = request.certificate.as_deref()
            && !reconcilers
                .iter()
                .any(|reconciler| reconciler.status().certificate == name)
            && !managed_reconcilers
                .iter()
                .any(|reconciler| reconciler.status().certificate == name)
        {
            return ApiResponse::error(404, "certificate_not_found", "certificate was not found");
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
                        outcomes.push(TlsReconcileOutcome::certbot(
                            status,
                            outcome.code(),
                            previous.map(|value| value.to_string()),
                            archive.to_string(),
                        ));
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
                    Ok(outcome) => outcomes.push(TlsReconcileOutcome::managed(
                        reconciler.status(),
                        outcome.code(),
                    )),
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
        dto_response(200, &TlsReconcileResponse::new(outcomes))
    }

    async fn tls_renew(&self, session: &mut ServerSession, context: &AuditContext) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
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
                dto_response(200, &TlsRenewResponse::new(status, outcome.code()))
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

    async fn tls_revoke(&self, session: &mut ServerSession, context: &AuditContext) -> ApiResponse {
        let request: TlsRevokeRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
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
                dto_response(
                    200,
                    &TlsActionResponse::new(certificate, outcome.code(), Some(job_id)),
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
            Err(_) => {
                ApiResponse::error(503, "revocation_worker_failed", "revocation worker failed")
            }
        }
    }

    async fn tls_delete(&self, session: &mut ServerSession, context: &AuditContext) -> ApiResponse {
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
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let active = mutation.generation();
        if active
            .config()
            .as_draft()
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
            .tls()
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
                dto_response(
                    200,
                    &TlsActionResponse::new(certificate, outcome.code(), Some(job_id)),
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
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
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
            let result = worker_reconciler.rollover_account_key_with_correlation(correlation_id);
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
                dto_response(
                    200,
                    &TlsActionResponse::new(certificate, outcome.code(), Some(job_id)),
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
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
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
                TlsJobControlResponse::cancellation(certificate.clone(), job_id)
            }),
            JobControl::Pause => reconciler.pause().map(|job_id| {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "applied",
                    &certificate,
                    context,
                );
                TlsJobControlResponse::paused(certificate.clone(), job_id)
            }),
            JobControl::Resume => reconciler.resume().map(|()| {
                crate::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "applied",
                    &certificate,
                    context,
                );
                TlsJobControlResponse::resumed(certificate.clone())
            }),
        };
        match result {
            Ok(response) => dto_response(202, &response),
            Err(error) => managed_error_response(error, "managed ACME job control failed"),
        }
    }

    fn audit_status(&self) -> ApiResponse {
        dto_response(200, &AuditStatusResponse::from(self.audit.status()))
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
        dto_response(200, &AuditPageResponse::from(page))
    }

    fn events(version: EventApiVersion, query: Option<&str>) -> ApiResponse {
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
        let page = crate::operational_event::page(after, limit);
        match version {
            EventApiVersion::V1 => dto_response(200, &EventPageV1Response::from(page)),
            EventApiVersion::V2 => dto_response(200, &EventPageV2Response::from(page)),
        }
    }
}

fn find_pool(active: &RuntimeGeneration, name: &str) -> Option<Arc<crate::RoundRobinPool>> {
    active
        .pools()
        .iter()
        .find(|pool| pool.health_snapshot().name == name)
        .cloned()
}

fn mutation_response(operation: &str, changed: usize) -> ApiResponse {
    crate::operational_event::emit(operation, "applied", None);
    dto_response(200, &MutationResponse::applied(changed))
}

fn dto_response<T: schemars::JsonSchema + Serialize>(status: u16, response: &T) -> ApiResponse {
    ApiResponse::json(
        status,
        &serde_json::to_value(response).expect("management response DTO serializes"),
    )
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

fn begin_mutation(
    generations: &GenerationManager,
    expected_revision: &str,
) -> Result<crate::GenerationMutation, ApiResponse> {
    let expected_revision = expected_revision
        .parse()
        .map_err(|_| mutation_error(&crate::GenerationError::RevisionConflict))?;
    generations
        .begin_mutation(&expected_revision)
        .map_err(|error| mutation_error(&error))
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
        crate::AcmeManagedError::Protocol(oxiroute_acme::AcmeError::InvalidRevocationReason) => 400,
        crate::AcmeManagedError::State(oxiroute_acme::AcmeStateError::PendingDnsCleanup)
        | crate::AcmeManagedError::Busy
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
    use std::{sync::Arc, time::Duration};

    use oxiroute_supervision::{LifecycleControl, LifecycleRequest};

    use super::{EventApiVersion, ManagementState, Route, match_route};
    use crate::{
        GenerationManager, RuntimeMetrics,
        lifecycle_control::{LifecycleError, LifecycleOutcome, LifecyclePort},
        operational_event::{AuditLimits, AuditStore},
    };

    struct ContractLifecycleControl(GenerationManager);

    impl LifecycleControl for ContractLifecycleControl {
        type Revision = String;
        type Status = crate::GenerationStatus;
        type Outcome = LifecycleOutcome;
        type Error = LifecycleError;

        fn status(&self) -> Self::Status {
            self.0.status()
        }

        fn execute(
            &self,
            _request: LifecycleRequest<Self::Revision>,
            _timeout: Option<Duration>,
        ) -> Result<Self::Outcome, Self::Error> {
            Err(LifecycleError::ShutdownUnavailable)
        }
    }

    #[test]
    fn management_state_compiles_against_lifecycle_port_without_direct_adapter() {
        let generations = GenerationManager::new();
        let lifecycle: Arc<LifecyclePort> = Arc::new(ContractLifecycleControl(generations.clone()));

        let _state = ManagementState::new(
            generations,
            lifecycle,
            RuntimeMetrics::new(),
            Arc::new(AuditStore::memory(AuditLimits::default())),
        );
    }

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
    fn event_pages_include_the_authoritative_latest_cursor() {
        let response = super::ManagementState::events(EventApiVersion::V2, Some("after=0&limit=1"));
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("event JSON");

        assert!(body["latestCursor"].is_u64());
    }

    #[test]
    fn version_one_event_pages_keep_the_shipped_shape() {
        let response = super::ManagementState::events(EventApiVersion::V1, Some("after=0&limit=1"));
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("event JSON");

        assert!(body.get("latestCursor").is_none());
    }

    #[test]
    fn version_two_event_page_and_stream_routes_are_exact() {
        assert!(matches!(
            match_route("/api/v1/events?after=4&limit=2"),
            Some(Route::Events(EventApiVersion::V1, Some("after=4&limit=2")))
        ));
        assert!(matches!(
            match_route("/api/v2/events?after=4&limit=2"),
            Some(Route::Events(EventApiVersion::V2, Some("after=4&limit=2")))
        ));
        assert!(matches!(
            match_route("/api/v2/events/stream?after=4&limit=2"),
            Some(Route::EventStream(
                EventApiVersion::V2,
                Some("after=4&limit=2")
            ))
        ));
        assert!(match_route("/api/v2/events/").is_none());
        assert!(match_route("/api/v2/events/stream/").is_none());
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
            crate::AcmeManagedError::Protocol(oxiroute_acme::AcmeError::InvalidRevocationReason),
            "revocation failed",
        );
        assert_eq!(response.status, 400);
    }

    #[test]
    fn unresolved_dns_cleanup_blocks_state_deletion_with_a_conflict() {
        let response = super::managed_error_response(
            crate::AcmeManagedError::State(oxiroute_acme::AcmeStateError::PendingDnsCleanup),
            "deletion failed",
        );
        assert_eq!(response.status, 409);
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("error JSON");
        assert_eq!(body["error"]["code"], "dns_cleanup_pending");
    }
}
