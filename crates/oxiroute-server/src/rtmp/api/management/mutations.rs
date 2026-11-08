#[allow(clippy::wildcard_imports)]
use super::*;

impl ManagementState {
    pub(super) async fn listener_state(&self, session: &mut ServerSession) -> ApiResponse {
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

    pub(super) async fn pool_state(&self, session: &mut ServerSession) -> ApiResponse {
        let request: PoolStateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let selected = match select_pools(mutation.generation(), &request.pools) {
            Ok(selected) => selected,
            Err(response) => return response,
        };
        for pool in selected {
            pool.set_administrative_state(request.state);
        }
        mutation_response("pool_administrative_state", request.pools.len())
    }

    pub(super) async fn server_state(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerStateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            self.set_server_state(pool, server, request.state)
        })
    }

    pub(super) async fn server_evacuate(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: ServerEvacuateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let (_mutation, selected) = match self.select_servers(&request.change) {
            Ok(selected) => selected,
            Err(response) => return response,
        };
        let mut total = EvacuationReport::default();
        for (pool, server) in &selected {
            let Ok(report) = self.evacuate_server(pool, server, request.force) else {
                return ApiResponse::error(
                    500,
                    "server_update_failed",
                    "server state could not be changed",
                );
            };
            emit_server_evacuation(context, pool.name(), Some(server), request.force, &report);
            total.merge(report);
        }
        dto_response(200, &EvacuationResponse::evacuated(request.force, &total))
    }

    pub(super) async fn pool_evacuate(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: PoolEvacuateRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let pools = match select_pools(mutation.generation(), &request.pools) {
            Ok(pools) => pools,
            Err(response) => return response,
        };
        let mut total = EvacuationReport::default();
        for pool in pools {
            let report = pool.evacuate_all_servers(request.force);
            emit_server_evacuation(context, pool.name(), None, request.force, &report);
            total.merge(report);
        }
        dto_response(200, &EvacuationResponse::evacuated(request.force, &total))
    }

    /// Drains through the registry operator state for a dynamic server, so the state is stored with
    /// its registration, and directly on the pool otherwise.
    fn set_server_state(
        &self,
        pool: &Arc<crate::RoundRobinPool>,
        server: &str,
        state: AdministrativeState,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match (pool.member(server), self.generations.registry()) {
            (Some((crate::ServerOrigin::Dynamic, _)), Some(registry)) => registry
                .set_operator_state(
                    &crate::registry::auth::Principal::admin("management"),
                    pool.name(),
                    server,
                    state,
                )
                .map(drop)
                .map_err(Box::<dyn std::error::Error>::from),
            _ => pool
                .set_server_administrative_state(server, state)
                .map_err(Box::<dyn std::error::Error>::from),
        }
    }

    fn evacuate_server(
        &self,
        pool: &Arc<crate::RoundRobinPool>,
        server: &str,
        force: bool,
    ) -> Result<EvacuationReport, Box<dyn std::error::Error>> {
        self.set_server_state(pool, server, AdministrativeState::Drain)?;
        Ok(pool.cancel_server_requests(server, force)?)
    }

    pub(super) async fn server_health(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerHealthRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_health_override(server, request.health)
        })
    }

    pub(super) async fn server_checks(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerChecksRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_checks_enabled(server, request.enabled)
        })
    }

    pub(super) async fn server_max_connections(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerCapacityRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_max_connections(server, request.max_connections)
        })
    }

    pub(super) async fn server_weight(&self, session: &mut ServerSession) -> ApiResponse {
        let request: ServerWeightRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        if request
            .weight
            .is_some_and(|weight| !(1..=100).contains(&weight))
        {
            return ApiResponse::error(400, "invalid_weight", "weight must be between 1 and 100");
        }
        self.apply_servers(&request.change, |pool, server| {
            pool.set_server_weight(server, request.weight)
        })
    }

    pub(super) async fn server_refresh_dns(&self, session: &mut ServerSession) -> ApiResponse {
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

    fn apply_servers<E>(
        &self,
        change: &ServerChange,
        apply: impl Fn(&Arc<crate::RoundRobinPool>, &str) -> Result<(), E>,
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

    pub(super) async fn generation_reload(&self, session: &mut ServerSession) -> ApiResponse {
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

    pub(super) async fn generation_rollback(&self, session: &mut ServerSession) -> ApiResponse {
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

    pub(super) async fn generation_drain(&self, session: &mut ServerSession) -> ApiResponse {
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

    pub(super) async fn process_drain(&self, session: &mut ServerSession) -> ApiResponse {
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

    pub(super) async fn process_shutdown(&self, session: &mut ServerSession) -> ApiResponse {
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
}
