#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn nonempty<T>(items: impl IntoIterator<Item = T>) -> Result<Vec<T>, PoolError> {
    let items = items.into_iter().collect::<Vec<_>>();
    if items.is_empty() {
        Err(PoolError::Empty)
    } else {
        Ok(items)
    }
}

const MAX_SERVER_WEIGHT: u16 = 100;

impl EndpointPool {
    pub(crate) fn acquire_compiled(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        startup: Option<HealthStartup>,
        queue_timeout: Option<Duration>,
        passive_policy: PassiveFailurePolicy,
        resilience: PoolResiliencePolicy,
        blueprint: &PoolConstructionBlueprint,
    ) -> Self {
        let endpoints = servers
            .into_iter()
            .zip(blueprint.weights.iter().copied())
            .map(|(server, weight)| {
                Arc::new(PoolEndpoint::new(
                    server,
                    startup,
                    weight,
                    ServerOrigin::Static,
                    0,
                    passive_policy.circuit_breaker,
                ))
            })
            .collect::<Vec<_>>();
        let queue = Arc::new(PoolQueue::new(
            queue_timeout.is_some(),
            blueprint.queue.clone(),
        ));
        let health = Arc::new(PoolHealthState::new(
            name.clone(),
            Membership::new(endpoints),
            Arc::clone(&queue),
            passive_policy,
            resilience,
            blueprint.readmission.clone(),
        ));
        Self {
            algorithm: blueprint.algorithm.clone(),
            name,
            startup,
            health,
            selection: Mutex::new(SelectionState::new(blueprint.weights.len())),
            queue,
            queue_timeout,
            resilience,
            unavailable_selections: AtomicU64::new(0),
            fail_open_selections: AtomicU64::new(0),
        }
    }

    /// Creates an unchecked round-robin pool from socket addresses.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an address cannot be represented by the runtime.
    pub fn new(endpoints: impl IntoIterator<Item = SocketAddr>) -> Result<Self, PoolError> {
        Self::from_endpoints(
            endpoints.into_iter().map(RuntimeEndpoint::from),
            UpstreamAlgorithm::RoundRobin,
        )
    }

    /// Creates an unchecked pool using the configured balancing algorithm.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an endpoint cannot be represented by the runtime.
    pub fn from_endpoints(
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
    ) -> Result<Self, PoolError> {
        Self::new_named(String::new(), nonempty(endpoints)?, algorithm, false)
    }

    /// Creates an unchecked pool with an immutable passive failure policy.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Empty`] when no endpoints are provided or a typed endpoint error when
    /// an endpoint cannot be represented by the runtime.
    pub fn from_endpoints_with_policy(
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
        passive_policy: PassiveFailurePolicy,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_with_policy(
            String::new(),
            nonempty(endpoints)?
                .into_iter()
                .enumerate()
                .map(|(index, endpoint)| RuntimeServer {
                    name: index.to_string(),
                    endpoint,
                    max_connections: None,
                    pinned_addresses: None,
                    protected_addresses: Arc::from([]),
                }),
            algorithm,
            None,
            None,
            passive_policy,
        )
    }

    pub(crate) fn new_named(
        name: String,
        endpoints: impl IntoIterator<Item = RuntimeEndpoint>,
        algorithm: UpstreamAlgorithm,
        checked: bool,
    ) -> Result<Self, PoolError> {
        let servers = endpoints
            .into_iter()
            .enumerate()
            .map(|(index, endpoint)| RuntimeServer {
                name: index.to_string(),
                endpoint,
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            });
        Self::new_named_servers(
            name,
            servers,
            algorithm,
            checked.then_some(HealthStartup::Checking),
            None,
        )
    }

    pub(crate) fn new_named_servers(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_with_policy(
            name,
            servers,
            algorithm,
            startup,
            queue_timeout,
            PassiveFailurePolicy::default(),
        )
    }

    pub(crate) fn new_named_servers_with_policy(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
        passive_policy: PassiveFailurePolicy,
    ) -> Result<Self, PoolError> {
        Self::new_named_servers_resilient(
            name,
            servers,
            algorithm,
            startup,
            queue_timeout,
            passive_policy,
            PoolResiliencePolicy::default(),
        )
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
    pub(crate) fn new_named_servers_resilient(
        name: String,
        servers: impl IntoIterator<Item = RuntimeServer>,
        algorithm: UpstreamAlgorithm,
        startup: Option<HealthStartup>,
        queue_timeout: Option<std::time::Duration>,
        passive_policy: PassiveFailurePolicy,
        resilience: PoolResiliencePolicy,
    ) -> Result<Self, PoolError> {
        passive_policy.validate()?;
        let servers = servers.into_iter().collect::<Vec<_>>();
        for server in &servers {
            server.endpoint.preflight()?;
        }
        let blueprint = PoolConstructionBlueprint::compile(&algorithm, servers.len(), None, None)?;
        Ok(Self::acquire_compiled(
            name,
            servers,
            startup,
            queue_timeout,
            passive_policy,
            resilience,
            &blueprint,
        ))
    }
}

mod admin;
mod evacuate;
mod selection;
mod snapshot;
