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
        let join = NewMember {
            server,
            spec,
            state: AdministrativeState::Ready,
        };
        self.add_servers(vec![join]).remove(0)
    }

    /// Adds several registered servers in one membership change, in the given order. Each one
    /// reports on its own whether it joined, as [`Self::add_server`] would.
    pub(crate) fn add_servers(
        &self,
        joining: Vec<NewMember>,
    ) -> Vec<Result<ServerHandle, PoolMembershipError>> {
        let mut selection = self.lock_selection();
        let current = self.members();
        let mut names = current
            .endpoints
            .iter()
            .map(|member| member.name.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut endpoints = current.endpoints.to_vec();
        let results = joining
            .into_iter()
            .map(|join| -> Result<ServerHandle, PoolMembershipError> {
                join.server.endpoint.preflight()?;
                if !names.insert(join.server.name.clone()) {
                    return Err(PoolMembershipError::DuplicateName(join.server.name));
                }
                let endpoint = Arc::new(PoolEndpoint::new(
                    &self.health.pool_name,
                    join.server,
                    self.startup,
                    join.spec.weight,
                    ServerOrigin::Dynamic,
                    join.spec.priority,
                    self.health.breaker_policy(),
                ));
                endpoint
                    .administrative_state
                    .store(join.state as u8, Ordering::Release);
                let at =
                    Membership::dynamic_position(&endpoints, join.spec.priority, &endpoint.name);
                endpoints.insert(at, Arc::clone(&endpoint));
                selection.insert_at(at);
                Ok(ServerHandle(endpoint))
            })
            .collect::<Vec<_>>();
        if results.iter().any(Result::is_ok) {
            self.publish(endpoints);
        }
        results
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
        selection.remove_at(at, endpoints.len());
        self.publish(endpoints);
        Ok(ServerHandle(removed))
    }

    /// Changes the weight, priority and registered capacity of a member without touching its
    /// health, leases or administrative state. Only a new priority moves the member in the
    /// rotation; an update that changes nothing wakes nobody.
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
        let at = current
            .position(name)
            .ok_or_else(|| PoolMembershipError::UnknownServer(name.to_owned()))?;
        let server = &current.endpoints[at];
        let limit = max_connections.unwrap_or(0);
        let capacity_changed = server.configured_weight() != spec.weight
            || server.configured_max_connections.load(Ordering::Acquire) != limit;
        let moved = server.priority() != spec.priority;
        if !capacity_changed && !moved {
            return Ok(());
        }
        server.set_configured_weight(spec.weight);
        server
            .configured_max_connections
            .store(limit, Ordering::Release);
        if moved {
            server.priority.store(spec.priority, Ordering::Release);
            let mut endpoints = current.endpoints.to_vec();
            let member = endpoints.remove(at);
            let to = Membership::dynamic_position(&endpoints, spec.priority, name);
            endpoints.insert(to, member);
            selection.move_to(at, to);
            self.publish(endpoints);
        } else {
            self.queue.notify_capacity_waiters();
        }
        Ok(())
    }

    fn lock_selection(&self) -> std::sync::MutexGuard<'_, SelectionState> {
        self.selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Publishes a new membership and wakes everything that waits on the old one.
    fn publish(&self, endpoints: Vec<Arc<PoolEndpoint>>) {
        self.health
            .members
            .store(Arc::new(Membership::new(endpoints)));
        self.queue.notify_capacity_waiters();
    }
}
