use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Weak},
};

use oxiroute_config::PoolRegistration;

use super::{RegistryService, State};
use crate::{
    HealthSupervisor, RoundRobinPool, RuntimeEndpoint, ServerOrigin,
    registry::{
        record::{Registration, RegistrationKey},
        store::StoredRegistry,
    },
    routing::{DynamicServer, NewMember, RuntimeServer},
};

/// One pool of one generation and what the registry may do to it.
pub struct PoolBinding {
    pool: Weak<RoundRobinPool>,
    pub(super) name: String,
    pub(super) policy: Option<PoolRegistration>,
    health: Option<HealthSupervisor>,
    pub(super) protected: Arc<[SocketAddr]>,
}

impl PoolBinding {
    #[must_use]
    pub fn new(
        pool: &Arc<RoundRobinPool>,
        policy: Option<PoolRegistration>,
        health: Option<HealthSupervisor>,
        protected: Arc<[SocketAddr]>,
    ) -> Self {
        Self {
            pool: Arc::downgrade(pool),
            name: pool.name().to_owned(),
            policy,
            health,
            protected,
        }
    }

    pub(super) fn live(&self) -> Option<Arc<RoundRobinPool>> {
        self.pool.upgrade()
    }
}

/// What a pool holds while a reconciliation decides who joins: member names and used endpoints.
struct Members {
    names: HashMap<String, (ServerOrigin, RuntimeEndpoint)>,
    endpoints: HashSet<RuntimeEndpoint>,
}

impl Members {
    fn of(pool: &RoundRobinPool) -> Self {
        let members = pool.health_snapshot().endpoints;
        Self {
            endpoints: members
                .iter()
                .map(|member| member.address.clone())
                .collect(),
            names: members
                .into_iter()
                .map(|member| (member.name, (member.origin, member.address)))
                .collect(),
        }
    }
}

impl RegistryService {
    pub(super) fn reconcile(binding: &PoolBinding, stored: &StoredRegistry) {
        let (Some(pool), Some(_)) = (binding.live(), &binding.policy) else {
            return;
        };
        let desired = stored
            .records
            .iter()
            .filter(|record| record.pool == binding.name)
            .collect::<Vec<_>>();
        let mut seen = Members::of(&pool);
        let mut joining = Vec::new();
        for record in &desired {
            let endpoint = RuntimeEndpoint::from(record.address);
            match seen.names.get(record.name.as_str()) {
                None if seen.endpoints.insert(endpoint.clone()) => {
                    seen.names.insert(
                        record.name.clone(),
                        (ServerOrigin::Dynamic, endpoint.clone()),
                    );
                    joining.push((
                        Self::new_member(binding, record, endpoint.clone()),
                        endpoint,
                    ));
                }
                Some((ServerOrigin::Dynamic, current)) if *current == endpoint => {
                    Self::apply(binding, &pool, record);
                }
                Some((ServerOrigin::Dynamic, _)) => {
                    Self::join_all(binding, &pool, &mut joining);
                    Self::apply(binding, &pool, record);
                    seen = Members::of(&pool);
                }
                Some((ServerOrigin::Static, _)) | None => {}
            }
        }
        Self::join_all(binding, &pool, &mut joining);
        let wanted = desired
            .iter()
            .map(|record| record.name.as_str())
            .collect::<HashSet<_>>();
        for member in pool.health_snapshot().endpoints {
            let stale =
                member.origin == ServerOrigin::Dynamic && !wanted.contains(member.name.as_str());
            if stale {
                Self::detach(binding, &pool, &member.name);
            }
        }
    }

    fn new_member(
        binding: &PoolBinding,
        record: &Registration,
        endpoint: RuntimeEndpoint,
    ) -> NewMember {
        NewMember {
            server: RuntimeServer {
                name: record.name.clone(),
                endpoint,
                max_connections: record.max_connections,
                pinned_addresses: None,
                protected_addresses: Arc::clone(&binding.protected),
            },
            spec: DynamicServer {
                weight: record.weight,
                priority: record.priority,
            },
            state: record.effective_state(),
        }
    }

    /// Adds the servers that are waiting to join with one membership change.
    fn join_all(
        binding: &PoolBinding,
        pool: &Arc<RoundRobinPool>,
        joining: &mut Vec<(NewMember, RuntimeEndpoint)>,
    ) {
        let (members, endpoints): (Vec<_>, Vec<_>) = joining.drain(..).unzip();
        let names = members
            .iter()
            .map(|member| member.server.name.clone())
            .collect::<Vec<_>>();
        let results = pool.add_servers(members);
        for ((result, name), endpoint) in results.into_iter().zip(names).zip(endpoints) {
            match result {
                Ok(handle) => {
                    if let Some(health) = &binding.health {
                        health.add_target(&binding.name, handle, endpoint);
                    }
                }
                Err(error) => log::warn!(
                    "registry: could not apply `{name}` to pool `{}`: {error}",
                    binding.name
                ),
            }
        }
    }

    fn apply(binding: &PoolBinding, pool: &Arc<RoundRobinPool>, record: &Registration) {
        let endpoint = RuntimeEndpoint::from(record.address);
        let spec = DynamicServer {
            weight: record.weight,
            priority: record.priority,
        };
        let existing = pool.member(&record.name);
        let result = match &existing {
            Some((ServerOrigin::Static, _)) => return,
            Some((ServerOrigin::Dynamic, current)) if *current == endpoint => pool
                .update_server(&record.name, spec, record.max_connections)
                .map_err(|error| error.to_string()),
            Some((ServerOrigin::Dynamic, _)) => {
                Self::detach(binding, pool, &record.name);
                Self::join(binding, pool, record, endpoint, spec)
            }
            None if pool.member_at(&endpoint).is_some() => return,
            None => Self::join(binding, pool, record, endpoint, spec),
        };
        match result {
            Ok(()) => {
                let _ =
                    pool.set_server_administrative_state(&record.name, record.effective_state());
            }
            Err(error) => log::warn!(
                "registry: could not apply `{}` to pool `{}`: {error}",
                record.name,
                binding.name
            ),
        }
    }

    fn join(
        binding: &PoolBinding,
        pool: &Arc<RoundRobinPool>,
        record: &Registration,
        endpoint: RuntimeEndpoint,
        spec: DynamicServer,
    ) -> Result<(), String> {
        let server = RuntimeServer {
            name: record.name.clone(),
            endpoint: endpoint.clone(),
            max_connections: record.max_connections,
            pinned_addresses: None,
            protected_addresses: Arc::clone(&binding.protected),
        };
        let handle = pool
            .add_server(server, spec)
            .map_err(|error| error.to_string())?;
        if let Some(health) = &binding.health {
            health.add_target(&binding.name, handle, endpoint);
        }
        Ok(())
    }

    fn detach(binding: &PoolBinding, pool: &Arc<RoundRobinPool>, name: &str) {
        if let Ok(handle) = pool.remove_server(name)
            && let Some(health) = &binding.health
        {
            health.remove_target(&binding.name, &handle);
        }
    }

    pub(super) fn apply_everywhere(state: &State, record: &Registration) {
        for binding in state
            .bindings
            .iter()
            .filter(|binding| binding.name == record.pool)
        {
            if let (Some(pool), Some(_)) = (binding.live(), &binding.policy) {
                Self::apply(binding, &pool, record);
            }
        }
    }

    pub(super) fn detach_everywhere(state: &State, key: &RegistrationKey) {
        for binding in state
            .bindings
            .iter()
            .filter(|binding| binding.name == key.pool)
        {
            if let (Some(pool), Some(_)) = (binding.live(), &binding.policy)
                && pool
                    .member(&key.name)
                    .is_some_and(|(origin, _)| origin == ServerOrigin::Dynamic)
            {
                Self::detach(binding, &pool, &key.name);
            }
        }
    }
}
