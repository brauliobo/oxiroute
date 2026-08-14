use crate::{ConfigDraft, ConfigError, Stats, ValidatedConfig};

#[derive(Debug, thiserror::Error)]
pub enum ConfigCompositionError {
    #[error("at least one canonical configuration is required")]
    Empty,
    #[error("canonical configurations disagree on process-wide field `{field}`")]
    ProcessFieldConflict { field: &'static str },
    #[error(transparent)]
    Invalid(#[from] ConfigError),
}

/// Composes complete configuration drafts in input order and returns validated state.
///
/// Process-wide settings must agree when more than one input specifies them. Draft fragments are
/// merged before canonical validation, allowing references to be completed by another draft.
///
/// # Errors
///
/// Returns an error when no input is supplied, process-wide fields conflict, or the resulting
/// canonical configuration is invalid.
pub fn compose_validated_configs(
    configs: Vec<ConfigDraft>,
) -> Result<ValidatedConfig, ConfigCompositionError> {
    compose_drafts(configs.into_iter())?
        .validate()
        .map_err(ConfigCompositionError::from)
}

fn compose_drafts(
    mut configs: impl Iterator<Item = ConfigDraft>,
) -> Result<ConfigDraft, ConfigCompositionError> {
    let Some(mut composed) = configs.next() else {
        return Err(ConfigCompositionError::Empty);
    };

    for config in configs {
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
        merge_stats(&mut composed.stats, config.stats.as_ref())?;
        composed.certificates.extend(config.certificates);
        composed.tls_profiles.extend(config.tls_profiles);
        composed.listeners.extend(config.listeners);
        composed.cache_stores.extend(config.cache_stores);
        composed.upstream_pools.extend(config.upstream_pools);
        composed.http_services.extend(config.http_services);
        composed
            .forward_proxy_services
            .extend(config.forward_proxy_services);
        composed.rtmp_services.extend(config.rtmp_services);
        composed.l4_services.extend(config.l4_services);
    }

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

fn merge_stats(
    target: &mut Option<Stats>,
    incoming: Option<&Stats>,
) -> Result<(), ConfigCompositionError> {
    let Some(incoming) = incoming else {
        return Ok(());
    };
    let Some(target) = target else {
        *target = Some(incoming.clone());
        return Ok(());
    };
    merge_optional(
        "stats.admin_token_file",
        &mut target.admin_token_file,
        incoming.admin_token_file.as_ref(),
    )?;
    target.binds.extend(incoming.binds.iter().copied());
    target.pages.extend(incoming.pages.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use crate::{
        DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpVersionPolicy, L4Service, Listener,
        ListenerBind, Protocol, StatsPage, StatsPageAdminPolicy, UpstreamAlgorithm,
        UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
    };

    use super::*;

    #[test]
    fn composes_namespaces_in_input_order() {
        let first = tcp_config("nginx", 80, 9080);
        let mut second = tcp_config("haproxy", 8080, 9081);
        second.max_connections = Some(4096);

        let composed = compose_validated_configs(vec![first, second]).expect("composed config");
        let composed = composed.as_draft();

        assert_eq!(composed.max_connections, Some(4096));
        assert_eq!(composed.listeners[0].name, "nginx");
        assert_eq!(composed.listeners[1].name, "haproxy");
    }

    #[test]
    fn composes_independent_stats_binds_and_pages() {
        let mut first = tcp_config("nginx", 80, 9080);
        first.stats = Some(Stats {
            binds: vec!["127.0.0.1:9000".parse().expect("stats bind")],
            admin_token_file: None,
            pages: Vec::new(),
        });
        let mut second = tcp_config("haproxy", 8080, 9081);
        second.stats = Some(Stats {
            binds: Vec::new(),
            admin_token_file: None,
            pages: vec![StatsPage {
                bind: "127.0.0.1:9001".parse().expect("stats page bind"),
                uri_prefix: "/stats".into(),
                refresh_ms: 10_000,
                admin: StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
        });

        let composed = compose_validated_configs(vec![first, second]).expect("composed stats");
        let composed = composed.as_draft();
        let stats = composed.stats.as_ref().expect("stats process");
        assert_eq!(stats.binds.len(), 1);
        assert_eq!(stats.pages.len(), 1);
    }

    #[test]
    fn rejects_conflicting_process_fields() {
        let mut first = empty_config();
        first.max_connections = Some(1024);
        let mut second = empty_config();
        second.max_connections = Some(4096);

        assert!(matches!(
            compose_validated_configs(vec![first, second]),
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
            compose_validated_configs(vec![first, second]),
            Err(ConfigCompositionError::Invalid(ConfigError::DuplicateName {
                namespace: "listener",
                name
            })) if name == "shared"
        ));
    }

    #[test]
    fn validates_only_after_complete_drafts_are_composed() {
        let mut listener = tcp_config("edge", 80, 9080);
        let mut services = empty_config();
        services.upstream_pools = std::mem::take(&mut listener.upstream_pools);
        services.l4_services = std::mem::take(&mut listener.l4_services);

        assert!(listener.clone().validate().is_err());
        assert!(services.clone().validate().is_ok());

        let composed =
            compose_validated_configs(vec![listener, services]).expect("complete composition");
        let composed = composed.as_draft();
        assert_eq!(composed.listeners.len(), 1);
        assert_eq!(composed.upstream_pools.len(), 1);
        assert_eq!(composed.l4_services.len(), 1);
    }

    fn empty_config() -> ConfigDraft {
        ConfigDraft {
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

    fn tcp_config(name: &str, port: u16, upstream_port: u16) -> ConfigDraft {
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
            passive_health: None,
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
            proxy_protocol: None,
            udp: None,
        });
        config.listeners.push(Listener {
            name: name.into(),
            bind: ListenerBind::Socket {
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            },
            protocol: Protocol::Tcp,
            service: Some(service),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        });
        config
    }
}
