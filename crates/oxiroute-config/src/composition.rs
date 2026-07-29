use crate::{Config, ConfigError, validate_config};

#[derive(Debug, thiserror::Error)]
pub enum ConfigCompositionError {
    #[error("at least one canonical configuration is required")]
    Empty,
    #[error("canonical configurations disagree on process-wide field `{field}`")]
    ProcessFieldConflict { field: &'static str },
    #[error(transparent)]
    Invalid(#[from] ConfigError),
}

/// Composes independently finalized canonical configurations in input order.
///
/// Process-wide settings must agree when more than one input specifies them. Named runtime
/// objects remain distinct and the normal canonical validator rejects collisions or dangling
/// references after composition.
///
/// # Errors
///
/// Returns an error when no input is supplied, process-wide fields conflict, or the resulting
/// canonical configuration is invalid.
pub fn compose_configs(configs: &[Config]) -> Result<Config, ConfigCompositionError> {
    let Some((first, remainder)) = configs.split_first() else {
        return Err(ConfigCompositionError::Empty);
    };
    let mut composed = first.clone();

    for config in remainder {
        if composed.version != config.version {
            return Err(ConfigCompositionError::ProcessFieldConflict { field: "version" });
        }
        merge_optional_copy(
            "max_connections",
            &mut composed.max_connections,
            config.max_connections,
        )?;
        merge_optional(
            "management",
            &mut composed.management,
            config.management.as_ref(),
        )?;
        merge_optional("stats", &mut composed.stats, config.stats.as_ref())?;
        composed.certificates.extend(config.certificates.clone());
        composed.tls_profiles.extend(config.tls_profiles.clone());
        composed.listeners.extend(config.listeners.clone());
        composed.cache_stores.extend(config.cache_stores.clone());
        composed
            .upstream_pools
            .extend(config.upstream_pools.clone());
        composed.http_services.extend(config.http_services.clone());
        composed
            .forward_proxy_services
            .extend(config.forward_proxy_services.clone());
        composed.rtmp_services.extend(config.rtmp_services.clone());
        composed.l4_services.extend(config.l4_services.clone());
    }

    validate_config(&mut composed)?;
    Ok(composed)
}

fn merge_optional_copy<T: Copy + Eq>(
    field: &'static str,
    target: &mut Option<T>,
    incoming: Option<T>,
) -> Result<(), ConfigCompositionError> {
    match (*target, incoming) {
        (None, Some(value)) => *target = Some(value),
        (Some(current), Some(value)) if current != value => {
            return Err(ConfigCompositionError::ProcessFieldConflict { field });
        }
        _ => {}
    }
    Ok(())
}

fn merge_optional<T: Clone + Eq>(
    field: &'static str,
    target: &mut Option<T>,
    incoming: Option<&T>,
) -> Result<(), ConfigCompositionError> {
    match (&*target, incoming) {
        (None, Some(value)) => *target = Some(value.clone()),
        (Some(current), Some(value)) if current != value => {
            return Err(ConfigCompositionError::ProcessFieldConflict { field });
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use crate::{
        DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpVersionPolicy, L4Service, Listener,
        ListenerBind, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint,
        UpstreamPool, UpstreamServer,
    };

    use super::*;

    #[test]
    fn composes_namespaces_in_input_order() {
        let first = tcp_config("nginx", 80, 9080);
        let mut second = tcp_config("haproxy", 8080, 9081);
        second.max_connections = Some(4096);

        let composed = compose_configs(&[first, second]).expect("composed config");

        assert_eq!(composed.max_connections, Some(4096));
        assert_eq!(composed.listeners[0].name, "nginx");
        assert_eq!(composed.listeners[1].name, "haproxy");
    }

    #[test]
    fn rejects_conflicting_process_fields() {
        let mut first = empty_config();
        first.max_connections = Some(1024);
        let mut second = empty_config();
        second.max_connections = Some(4096);

        assert!(matches!(
            compose_configs(&[first, second]),
            Err(ConfigCompositionError::ProcessFieldConflict {
                field: "max_connections"
            })
        ));
    }

    #[test]
    fn validates_the_composed_namespace() {
        let first = tcp_config("shared", 80, 9080);
        let second = tcp_config("shared", 8080, 9081);

        assert!(matches!(
            compose_configs(&[first, second]),
            Err(ConfigCompositionError::Invalid(ConfigError::DuplicateName {
                namespace: "listener",
                name
            })) if name == "shared"
        ));
    }

    fn empty_config() -> Config {
        Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        }
    }

    fn tcp_config(name: &str, port: u16, upstream_port: u16) -> Config {
        let mut config = empty_config();
        let pool = format!("{name}-pool");
        let service = format!("{name}-service");
        config.upstream_pools.push(UpstreamPool {
            name: pool.clone(),
            servers: vec![UpstreamServer {
                name: format!("{name}-server"),
                endpoint: UpstreamEndpoint::Socket {
                    address: SocketAddr::from((Ipv4Addr::LOCALHOST, upstream_port)),
                },
                max_connections: None,
                dns_resolution: DnsResolutionPolicy::default(),
            }],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::default(),
            health_check: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
        });
        config.l4_services.push(L4Service {
            name: service.clone(),
            upstream_pool: pool,
            connect_timeout_ms: 5000,
            idle_timeout_ms: 60_000,
            lifetime_timeout_ms: None,
        });
        config.listeners.push(Listener {
            name: name.into(),
            bind: ListenerBind::Socket {
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            },
            protocol: Protocol::Tcp,
            service: Some(service),
            tls_profile: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        });
        config
    }
}
