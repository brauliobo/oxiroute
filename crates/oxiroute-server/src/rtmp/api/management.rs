use std::{sync::Arc, time::Duration};

use oxiroute_config::CertificateSource;
use pingora::protocols::http::ServerSession;
use serde::{Deserialize, Serialize};

use super::{
    ApiResponse,
    config::read_config_body,
    dto::{
        AuditPageResponse, AuditStatusResponse, ConfigRejectedResponse, DnsRefreshResponse,
        DnsRefreshServer, DrainRequest, DrainResponse, EvacuationResponse, EventPageV1Response,
        EventPageV2Response, GenerationActionResponse, GenerationResponse,
        ListenerInventoryResponse, ListenerStateRequest, MutationResponse, PoolEvacuateRequest,
        PoolStateRequest, ProcessMutationResponse, RevisionRequest, ServerCapacityRequest,
        ServerChange, ServerChecksRequest, ServerEvacuateRequest, ServerHealthRequest,
        ServerStateRequest, ServerWeightRequest, TlsActionResponse, TlsCertificateDto,
        TlsInventoryResponse, TlsJobControlResponse, TlsReconcileOutcome, TlsReconcileResponse,
        TlsRenewResponse, TlsRequest, TlsRevokeRequest, pools_body, servers_body,
    },
};
use crate::{
    AdministrativeState, EvacuationReport, GenerationManager, RuntimeGeneration, RuntimeMetrics,
    lifecycle_control::{LifecycleError, LifecycleOutcome, LifecyclePort},
    observability::operational_event::{
        AuditCategory, AuditContext, AuditResult, AuditStore, emit_server_evacuation,
    },
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
            (Route::PoolEvacuate, "POST") => self.pool_evacuate(session, context).await,
            (Route::Servers, "GET") => self.servers(),
            (Route::ServerState, "POST") => self.server_state(session).await,
            (Route::ServerEvacuate, "POST") => self.server_evacuate(session, context).await,
            (Route::ServerHealth, "POST") => self.server_health(session).await,
            (Route::ServerChecks, "POST") => self.server_checks(session).await,
            (Route::ServerMaxConnections, "PUT") => self.server_max_connections(session).await,
            (Route::ServerWeight, "PUT") => self.server_weight(session).await,
            (Route::ServerRefreshDns, "POST") => self.server_refresh_dns(session).await,
            (Route::Generations, "GET") => {
                ApiResponse::dto(200, &GenerationResponse::from(self.lifecycle.status()))
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
            (Route::ServerMaxConnections | Route::ServerWeight, _) => {
                ApiResponse::method_not_allowed("PUT")
            }
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
}

fn find_pool(active: &RuntimeGeneration, name: &str) -> Option<Arc<crate::RoundRobinPool>> {
    active
        .pools()
        .iter()
        .find(|pool| pool.health_snapshot().name == name)
        .cloned()
}

fn select_pools(
    active: &RuntimeGeneration,
    names: &[String],
) -> Result<Vec<Arc<crate::RoundRobinPool>>, ApiResponse> {
    if names.is_empty() || names.len() > MAX_BATCH_TARGETS {
        return Err(invalid_batch());
    }
    names
        .iter()
        .map(|name| {
            find_pool(active, name).ok_or_else(|| {
                ApiResponse::error(404, "pool_not_found", "an upstream pool was not found")
            })
        })
        .collect()
}

fn mutation_response(operation: &str, changed: usize) -> ApiResponse {
    crate::observability::operational_event::emit(operation, "applied", None);
    dto_response(200, &MutationResponse::applied(changed))
}

fn dto_response<T: schemars::JsonSchema + Serialize>(status: u16, response: &T) -> ApiResponse {
    ApiResponse::dto(status, response)
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

async fn body<T: for<'de> Deserialize<'de>>(session: &mut ServerSession) -> Result<T, ApiResponse> {
    let bytes = read_config_body(session).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiResponse::error(400, "invalid_json", "request body is invalid JSON"))
}

mod mutations;
mod read;
mod routes;
mod tls;

pub(super) use routes::*;
use tls::JobControl;
#[cfg(test)]
use tls::managed_error_response;

#[cfg(test)]
mod tests;
