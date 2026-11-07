#[allow(clippy::wildcard_imports)]
use super::*;

impl EndpointPool {
    #[must_use]
    pub fn health_snapshot(&self) -> PoolHealthSnapshot {
        let now_unix_ms = now_unix_ms();
        let endpoints = self.read_health(|members| {
            members
                .endpoints
                .iter()
                .map(|server| server.snapshot(now_unix_ms))
                .collect::<Vec<_>>()
        });
        PoolHealthSnapshot {
            name: self.name.clone(),
            algorithm: algorithm_name(&self.algorithm),
            available_endpoints: endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.administrative_state == AdministrativeState::Ready
                        && !endpoint.passive_ejected
                        && match endpoint.health_override {
                            HealthOverride::Auto => endpoint.state.selectable(),
                            HealthOverride::Up => true,
                            HealthOverride::Down => false,
                        }
                })
                .count(),
            total_endpoints: endpoints.len(),
            unavailable_selections: self.unavailable_selections.load(Ordering::Relaxed),
            fail_open_selections: self.fail_open_selections.load(Ordering::Relaxed),
            queued: self.queue.queued.load(Ordering::Relaxed),
            queued_by_class: self.queue.depth_by_class(),
            oldest_queue_wait_ms: u64::try_from(self.queue.ledger.oldest().as_millis())
                .unwrap_or(u64::MAX),
            queued_total: self.queue.queued_total.load(Ordering::Relaxed),
            queue_timeouts: self.queue.timeouts.load(Ordering::Relaxed),
            queue_cancellations: self.queue.cancellations.load(Ordering::Relaxed),
            canary: self.health.canary.get().map(|state| state.snapshot()),
            endpoints,
        }
    }

    pub(super) fn read_health<T>(&self, read: impl Fn(&Membership) -> T) -> T {
        self.read_health_of(&self.members(), read)
    }

    pub(super) fn read_health_of<T>(
        &self,
        members: &Membership,
        read: impl Fn(&Membership) -> T,
    ) -> T {
        for _ in 0..8 {
            let version = self.health.health_version.load(Ordering::Acquire);
            if !version.is_multiple_of(2) {
                std::thread::yield_now();
                continue;
            }
            let value = read(members);
            fence(Ordering::Acquire);
            if self.health.health_version.load(Ordering::Relaxed) == version {
                return value;
            }
        }
        let _writer = self
            .health
            .health_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read(members)
    }

    /// Adds a server that registered at runtime. Static servers stay first in file order.
    ///
    /// # Errors
    ///
    /// Returns an error for a name already in the pool or an endpoint the runtime cannot represent.
    pub(crate) fn add_server(
        &self,
        server: RuntimeServer,
        spec: DynamicServer,
    ) -> Result<ServerHandle, PoolMembershipError> {
        server.endpoint.preflight()?;
        let mut selection = self.lock_selection();
        let current = self.members();
        if current.position(&server.name).is_some() {
            return Err(PoolMembershipError::DuplicateName(server.name));
        }
        let endpoint = Arc::new(PoolEndpoint::new(
            server,
            self.startup,
            spec.weight,
            ServerOrigin::Dynamic,
            spec.priority,
            self.health.breaker_policy(),
        ));
        let mut endpoints = current.endpoints.to_vec();
        endpoints.insert(
            current.dynamic_position(spec.priority, &endpoint.name),
            Arc::clone(&endpoint),
        );
        self.install(&mut selection, &current, endpoints);
        Ok(ServerHandle(endpoint))
    }

    /// Removes a server from selection. Leases it already handed out keep counting against it.
    ///
    /// # Errors
    ///
    /// Returns an error when the pool has no server with that name.
    pub(crate) fn remove_server(&self, name: &str) -> Result<ServerHandle, PoolMembershipError> {
        let mut selection = self.lock_selection();
        let current = self.members();
        let at = current
            .position(name)
            .ok_or_else(|| PoolMembershipError::UnknownServer(name.to_owned()))?;
        let removed = Arc::clone(&current.endpoints[at]);
        let mut endpoints = current.endpoints.to_vec();
        endpoints.remove(at);
        self.install(&mut selection, &current, endpoints);
        Ok(ServerHandle(removed))
    }

    /// Changes the weight, priority and registered capacity of a member without touching its
    /// health, leases or administrative state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown name.
    pub(crate) fn update_server(
        &self,
        name: &str,
        spec: DynamicServer,
        max_connections: Option<u64>,
    ) -> Result<(), PoolMembershipError> {
        let mut selection = self.lock_selection();
        let current = self.members();
        let server = self
            .server_endpoint(name)
            .ok_or_else(|| PoolMembershipError::UnknownServer(name.to_owned()))?;
        server.set_configured_weight(spec.weight);
        server.priority.store(spec.priority, Ordering::Release);
        server
            .configured_max_connections
            .store(max_connections.unwrap_or(0), Ordering::Release);
        let mut endpoints = current.endpoints.to_vec();
        let statics = endpoints
            .iter()
            .filter(|member| member.origin == ServerOrigin::Static)
            .count();
        endpoints[statics..].sort_by(|a, b| (a.priority(), &a.name).cmp(&(b.priority(), &b.name)));
        self.install(&mut selection, &current, endpoints);
        Ok(())
    }

    fn lock_selection(&self) -> std::sync::MutexGuard<'_, SelectionState> {
        self.selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Publishes a new membership, keeping each surviving server's weighted credit and the
    /// rotation cursor on the same server, and wakes everything that waits on the old one.
    fn install(
        &self,
        selection: &mut SelectionState,
        previous: &Membership,
        endpoints: Vec<Arc<PoolEndpoint>>,
    ) {
        let position = |members: &[Arc<PoolEndpoint>], server: &Arc<PoolEndpoint>| {
            members
                .iter()
                .position(|member| Arc::ptr_eq(member, server))
        };
        selection.credit = endpoints
            .iter()
            .map(|server| {
                position(&previous.endpoints, server).map_or(0, |index| selection.credit[index])
            })
            .collect();
        let next = previous
            .endpoints
            .get(selection.next)
            .and_then(|server| position(&endpoints, server))
            .unwrap_or(selection.next);
        selection.next = next.checked_rem(endpoints.len()).unwrap_or(0);
        self.health
            .members
            .store(Arc::new(Membership::new(endpoints)));
        self.queue.notify_capacity_waiters();
    }
}
