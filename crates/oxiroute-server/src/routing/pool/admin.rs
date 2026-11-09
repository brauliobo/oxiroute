#[allow(clippy::wildcard_imports)]
use super::*;

impl EndpointPool {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn algorithm(&self) -> UpstreamAlgorithm {
        self.algorithm.clone()
    }

    #[must_use]
    pub fn has_unattempted(&self, attempted: &[RuntimeEndpoint]) -> bool {
        self.has_unattempted_by(|server| attempted.contains(&server.endpoint))
    }

    #[must_use]
    pub(crate) fn has_unattempted_servers(&self, attempted: &[String]) -> bool {
        self.has_unattempted_by(|server| attempted.contains(&server.name))
    }

    fn has_unattempted_by(&self, attempted: impl Fn(&PoolEndpoint) -> bool) -> bool {
        let now_unix_ms = now_unix_ms();
        let members = self.members();
        let view = self.health.selectable_view(&members, now_unix_ms);
        if !view.is_empty() {
            return view
                .rotation(0)
                .any(|index| !attempted(&members.endpoints[index]));
        }
        self.resilience.fail_open
            && members
                .endpoints
                .iter()
                .any(|server| !attempted(server) && server.admission(now_unix_ms, true).is_some())
    }

    #[must_use]
    pub fn has_available(&self) -> bool {
        let members = self.members();
        let view = self.health.selectable_view(&members, now_unix_ms());
        view.rotation(0)
            .any(|index| members.endpoints[index].has_capacity())
    }

    /// Whether any server could be chosen now, ignoring whether it has spare capacity.
    #[must_use]
    pub fn has_selectable(&self) -> bool {
        let members = self.members();
        !self
            .health
            .selectable_view(&members, now_unix_ms())
            .is_empty()
    }

    pub(crate) fn note_unavailable_selection(&self) {
        self.unavailable_selections.fetch_add(1, Ordering::Relaxed);
    }

    /// Changes one named server's administrative state. Existing leases continue draining.
    ///
    /// # Errors
    ///
    /// Returns an error when this pool has no server with the exact canonical name.
    pub fn set_server_administrative_state(
        &self,
        name: &str,
        state: AdministrativeState,
    ) -> Result<(), PoolAdminError> {
        let server = self.server(name)?;
        let previous = server
            .administrative_state
            .swap(state as u8, Ordering::AcqRel);
        if previous != state as u8 {
            self.health.selectable_changed();
            self.queue.notify_capacity_waiters();
        }
        Ok(())
    }

    pub fn set_administrative_state(&self, state: AdministrativeState) {
        for server in &*self.members().endpoints {
            server
                .administrative_state
                .store(state as u8, Ordering::Release);
        }
        self.health.selectable_changed();
        self.queue.notify_capacity_waiters();
    }

    /// Sets a selection-only health override without changing observed health.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact server name is unknown.
    pub fn set_server_health_override(
        &self,
        name: &str,
        health_override: HealthOverride,
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .health_override
            .store(health_override as u8, Ordering::Release);
        self.health.selectable_changed();
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Enables or disables configured health probes for one server.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact server name is unknown.
    pub fn set_server_checks_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .checks_enabled
            .store(enabled, Ordering::Release);
        Ok(())
    }

    /// Sets a runtime capacity override, or restores configured capacity with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error for zero capacity or an unknown exact server name.
    pub fn set_server_max_connections(
        &self,
        name: &str,
        limit: Option<u64>,
    ) -> Result<(), PoolAdminError> {
        if limit == Some(0) {
            return Err(PoolAdminError::InvalidMaxConnections);
        }
        let server = self.server(name)?;
        server
            .max_connections_override
            .store(limit.unwrap_or(0), Ordering::Release);
        self.queue.load_log.note(&server);
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Sets a runtime weight override, or restores the configured weight with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error for a weight outside 1..=100 or an unknown exact server name.
    pub fn set_server_weight(&self, name: &str, weight: Option<u16>) -> Result<(), PoolAdminError> {
        if weight.is_some_and(|weight| !(1..=MAX_SERVER_WEIGHT).contains(&weight)) {
            return Err(PoolAdminError::InvalidWeight);
        }
        self.server(name)?
            .weight_override
            .store(weight.unwrap_or(0), Ordering::Release);
        self.health.selectable_changed();
        self.queue.notify_capacity_waiters();
        Ok(())
    }

    /// Administrative state that differs from a freshly started server's.
    #[must_use]
    pub fn administrative_states(&self) -> Vec<ServerAdminState> {
        self.members()
            .endpoints
            .iter()
            .map(|server| ServerAdminState {
                pool: self.name.clone(),
                server: server.name.clone(),
                administrative: server.administrative_state(),
                health: server.health_override(),
                checks_enabled: server.checks_enabled.load(Ordering::Acquire),
                max_connections: match server.max_connections_override.load(Ordering::Acquire) {
                    0 => None,
                    limit => Some(limit),
                },
                weight: match server.weight_override.load(Ordering::Acquire) {
                    0 => None,
                    weight => Some(weight),
                },
            })
            .filter(|state| !state.is_default())
            .collect()
    }

    /// Applies carried state to the servers that still exist in this pool.
    pub fn restore_administrative_states(&self, states: &[ServerAdminState]) {
        for state in states.iter().filter(|state| state.pool == self.name) {
            let Ok(server) = self.server(&state.server) else {
                continue;
            };
            server
                .administrative_state
                .store(state.administrative as u8, Ordering::Release);
            server
                .health_override
                .store(state.health as u8, Ordering::Release);
            server
                .checks_enabled
                .store(state.checks_enabled, Ordering::Release);
            server
                .max_connections_override
                .store(state.max_connections.unwrap_or(0), Ordering::Release);
            server
                .weight_override
                .store(state.weight.unwrap_or(0), Ordering::Release);
        }
        self.health.selectable_changed();
        self.queue.notify_capacity_waiters();
    }

    #[must_use]
    pub fn has_server(&self, name: &str) -> bool {
        self.server_endpoint(name).is_some()
    }

    pub(super) fn server_endpoint(&self, name: &str) -> Option<Arc<PoolEndpoint>> {
        let members = self.members();
        members
            .position(name)
            .map(|index| Arc::clone(&members.endpoints[index]))
    }

    pub(super) fn server(&self, name: &str) -> Result<Arc<PoolEndpoint>, PoolAdminError> {
        self.server_endpoint(name)
            .ok_or_else(|| PoolAdminError::UnknownServer(name.to_owned()))
    }

    /// Origin and endpoint of the member with this name.
    pub(crate) fn member(&self, name: &str) -> Option<(ServerOrigin, RuntimeEndpoint)> {
        self.server_endpoint(name)
            .map(|server| (server.origin, server.endpoint.clone()))
    }

    /// Where every member comes from, by name.
    pub(crate) fn member_origins(&self) -> std::collections::HashMap<String, ServerOrigin> {
        self.members()
            .endpoints
            .iter()
            .map(|server| (server.name.clone(), server.origin))
            .collect()
    }

    /// The member that already uses `endpoint`, if any.
    pub(crate) fn member_at(&self, endpoint: &RuntimeEndpoint) -> Option<String> {
        self.members()
            .endpoints
            .iter()
            .find(|server| server.endpoint == *endpoint)
            .map(|server| server.name.clone())
    }

    #[cfg(test)]
    pub(crate) fn server_handle(&self, name: &str) -> Option<ServerHandle> {
        self.server_endpoint(name).map(ServerHandle)
    }

    pub(crate) fn endpoints(&self) -> impl Iterator<Item = (ServerHandle, RuntimeEndpoint)> {
        self.members()
            .endpoints
            .iter()
            .map(|server| (ServerHandle(Arc::clone(server)), server.endpoint.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[cfg(test)]
    pub(crate) fn handle(&self, index: usize) -> ServerHandle {
        ServerHandle(Arc::clone(&self.members().endpoints[index]))
    }

    pub(crate) fn record_health(
        &self,
        server: &ServerHandle,
        healthy: bool,
        failure: Option<HealthFailure>,
        at_unix_ms: Option<u64>,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
    ) -> Option<(EndpointHealthState, EndpointHealthState)> {
        self.health
            .record_health(
                &server.0,
                healthy,
                failure,
                at_unix_ms,
                healthy_threshold,
                unhealthy_threshold,
            )
            .transition
    }

    #[cfg(test)]
    pub(crate) fn record_passive_failure(&self, index: usize, failure: HealthFailure) {
        if let Some(server) = self.members().endpoints.get(index) {
            self.health.record_passive_failure(server, failure);
        }
    }

    #[cfg(test)]
    pub(in super::super) fn record_passive_failure_at(
        &self,
        index: usize,
        failure: HealthFailure,
        at_unix_ms: u64,
    ) {
        if let Some(server) = self.members().endpoints.get(index) {
            self.health
                .record_passive_failure_at(server, failure, at_unix_ms);
        }
    }

    /// Resolves one server immediately without mutating runtime state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server or failed/empty DNS resolution.
    pub async fn resolve_server_dns(&self, name: &str) -> Result<Vec<SocketAddr>, PoolAdminError> {
        let server = self.server(name)?;
        server
            .resolve_fresh_dns()
            .await
            .map_err(|_| PoolAdminError::DnsRefreshFailed)
    }

    /// Commits an already-resolved address set without performing external I/O.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown server, a non-DNS endpoint, or a protected address.
    pub fn commit_server_dns(
        &self,
        name: &str,
        addresses: &[SocketAddr],
    ) -> Result<(), PoolAdminError> {
        self.server(name)?
            .commit_dns(addresses)
            .map_err(|_| PoolAdminError::DnsRefreshFailed)
    }

    pub(crate) async fn resolve_server_addresses(
        &self,
        server: &ServerHandle,
    ) -> io::Result<Vec<SocketAddr>> {
        server.0.resolve_probe_addresses().await
    }
}
