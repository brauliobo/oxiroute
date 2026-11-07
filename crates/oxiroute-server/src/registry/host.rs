use std::{
    net::SocketAddr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use oxiroute_config::{ConfigDraft, ListenerBind, RegistryEndpoint};

use super::{
    lease::SystemClock,
    service::{PoolBinding, RegistryService},
};
use crate::RuntimeGeneration;

/// Owns the process's registry service. It starts with the first generation whose configuration
/// has a `registry` block, and every later generation is attached to the same service.
pub(crate) struct RegistryHost {
    service: OnceLock<Arc<RegistryService>>,
    writer: AtomicBool,
}

impl RegistryHost {
    /// A direct process writes from the start; a supervised worker waits for its activation.
    pub(crate) const fn new(writer: bool) -> Self {
        Self {
            service: OnceLock::new(),
            writer: AtomicBool::new(writer),
        }
    }

    pub(crate) fn service(&self) -> Option<Arc<RegistryService>> {
        self.service.get().cloned()
    }

    pub(crate) fn set_writer(&self, writer: bool) {
        self.writer.store(writer, Ordering::Release);
        if let Some(service) = self.service.get() {
            service.set_writer(writer);
        }
    }

    /// Seeds the pools of a generation that is about to serve with the registered servers.
    pub(crate) fn on_activate(&self, generation: &RuntimeGeneration) {
        let config = generation.config().as_draft();
        let service = match (self.service.get(), &config.registry) {
            (Some(service), _) => Arc::clone(service),
            (None, Some(registry)) => {
                match RegistryService::open(registry, Arc::new(SystemClock)) {
                    Ok(service) => {
                        service.set_writer(self.writer.load(Ordering::Acquire));
                        Arc::clone(self.service.get_or_init(|| service))
                    }
                    Err(error) => {
                        log::error!("registry disabled: {error}");
                        return;
                    }
                }
            }
            (None, None) => return,
        };
        service.attach(bindings(generation, config));
    }
}

fn bindings(generation: &RuntimeGeneration, config: &ConfigDraft) -> Vec<PoolBinding> {
    let protected: Arc<[SocketAddr]> = protected_addresses(config).into();
    generation
        .pools()
        .iter()
        .map(|pool| {
            let policy = config
                .upstream_pools
                .iter()
                .find(|configured| configured.name == pool.name())
                .and_then(|configured| configured.registration.clone());
            PoolBinding::new(
                pool,
                policy,
                generation.health_supervisor(),
                Arc::clone(&protected),
            )
        })
        .collect()
}

/// Addresses a registered server may never point at: every oxiroute listener and API bind.
pub(crate) fn protected_addresses(config: &ConfigDraft) -> Vec<SocketAddr> {
    let listeners = config
        .listeners
        .iter()
        .filter_map(|listener| match listener.bind {
            ListenerBind::Socket { address } | ListenerBind::Udp { address } => Some(address),
            ListenerBind::Unix { .. } => None,
        });
    let stats = config.stats.iter().flat_map(|stats| {
        stats
            .binds
            .iter()
            .copied()
            .chain(stats.pages.iter().map(|page| page.bind))
    });
    let registry = config.registry.iter().flat_map(|registry| {
        registry
            .endpoints
            .iter()
            .filter_map(|endpoint| match endpoint {
                RegistryEndpoint::Tcp { bind, .. } => Some(*bind),
                RegistryEndpoint::Unix { .. } => None,
            })
    });
    config
        .management
        .iter()
        .map(|management| management.bind)
        .chain(stats)
        .chain(listeners)
        .chain(registry_binds(config))
        .collect()
}

/// TCP binds of the registry-only endpoints.
pub(crate) fn registry_binds(config: &ConfigDraft) -> impl Iterator<Item = SocketAddr> + '_ {
    config.registry.iter().flat_map(|registry| {
        registry
            .endpoints
            .iter()
            .filter_map(|endpoint| match endpoint {
                RegistryEndpoint::Tcp { bind, .. } => Some(*bind),
                RegistryEndpoint::Unix { .. } => None,
            })
    })
}
