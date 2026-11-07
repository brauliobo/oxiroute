#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug)]
pub(crate) struct StaticPeerPlan {
    pub(super) host: Host,
    pub(super) port: u16,
}

#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ForwardServiceBlueprint {
    pub(crate) name: String,
    pub(crate) cache: Option<crate::generation::blueprint::CachePolicyBlueprint>,
    pub(super) auth: Option<ForwardAuthBlueprint>,
    pub(super) challenge: Option<HeaderValue>,
    pub(super) access_policy: Option<ForwardAccessPolicy>,
    pub(super) allow_absolute_form: bool,
    pub(super) audit_mode: ForwardAuditMode,
    pub(super) connect_enabled: bool,
    pub(super) connect_ports: Arc<[u16]>,
    pub(super) connect_udp_enabled: bool,
    pub(super) connect_udp_ports: Arc<[u16]>,
    pub(super) connect_timeout: Duration,
    pub(super) destination_policy: DestinationRules,
    pub(super) peer_direct_fallback: ForwardDirectFallback,
    pub(super) peer_max_retries: usize,
    pub(super) peers: Arc<[StaticPeerPlan]>,
    pub(super) header_policy: ForwardHeaderPolicy,
    pub(super) idle_timeout: Duration,
    pub(super) lifetime_timeout: Duration,
    pub(super) max_connections: usize,
    pub(super) max_header_bytes: usize,
    pub(super) max_request_body_bytes: usize,
    pub(super) resolver: ForwardResolverBlueprint,
    pub(super) resolver_addresses: usize,
    pub(super) resolver_queries: usize,
    pub(super) resolver_revalidate_on_connect: bool,
    pub(super) h3_upstream_connections: Option<usize>,
}

#[derive(Clone)]
pub(super) enum ForwardAuthBlueprint {
    Bearer {
        token_file_path: PathBuf,
    },
    Basic {
        htpasswd_file_path: PathBuf,
        realm: String,
        credential_ttl: Option<Duration>,
        username_case_sensitive: bool,
    },
}

#[derive(Clone)]
pub(super) struct ForwardResolverBlueprint {
    pub(super) nameservers: Arc<[IpAddr]>,
    pub(super) options: ResolverOpts,
}

impl ForwardServiceBlueprint {
    pub(crate) fn compile(
        service: &oxiroute_config::ForwardProxyService,
        cache: Option<crate::generation::blueprint::CachePolicyBlueprint>,
    ) -> Result<Self, ForwardPlanError> {
        let destination_policy = DestinationRules::new(
            service.destination_policy.allow_domains.clone(),
            service.destination_policy.deny_domains.clone(),
            service.destination_policy.allow_cidrs.clone(),
            service.destination_policy.deny_cidrs.clone(),
            service.destination_policy.deny_private,
        )
        .and_then(|policy| {
            policy.with_time_windows(
                destination_time_windows(&service.destination_policy.allow_times)?,
                destination_time_windows(&service.destination_policy.deny_times)?,
            )
        })
        .map_err(|_| ForwardPlanError::DestinationPolicy)?;
        let peers = service
            .peer_policy
            .peers
            .iter()
            .map(static_peer_plan)
            .collect::<Result<Vec<_>, _>>()?
            .into();
        let max_connections =
            usize::try_from(service.max_connections).map_err(|_| ForwardPlanError::Limit)?;
        let max_header_bytes =
            usize::try_from(service.max_header_bytes).map_err(|_| ForwardPlanError::Limit)?;
        let max_request_body_bytes = usize::try_from(
            service
                .max_request_body_bytes
                .ok_or(ForwardPlanError::Limit)?,
        )
        .map_err(|_| ForwardPlanError::Limit)?;
        let resolver_addresses = usize::try_from(service.resolver.max_addresses_per_name)
            .map_err(|_| ForwardPlanError::Limit)?;
        let resolver_queries = usize::try_from(service.resolver.max_concurrent_queries)
            .map_err(|_| ForwardPlanError::Limit)?;
        let (auth, challenge) = compile_forward_auth(service.auth.as_ref())?;
        let resolver = compile_resolver_blueprint(&service.resolver);
        Ok(Self {
            name: service.name.clone(),
            cache,
            auth,
            challenge,
            access_policy: service.access_policy.clone(),
            allow_absolute_form: service.allow_absolute_form,
            audit_mode: service.audit_mode,
            connect_enabled: service.connect.enabled,
            connect_ports: service.connect.allowed_ports.clone().into(),
            connect_udp_enabled: service.connect_udp.enabled,
            connect_udp_ports: service.connect_udp.allowed_ports.clone().into(),
            connect_timeout: Duration::from_millis(service.connect_timeout_ms),
            destination_policy,
            peer_direct_fallback: service.peer_policy.direct_fallback,
            peer_max_retries: usize::from(service.peer_policy.max_retries),
            peers,
            header_policy: service.header_policy.clone(),
            idle_timeout: Duration::from_millis(service.idle_timeout_ms),
            lifetime_timeout: Duration::from_millis(service.lifetime_timeout_ms),
            max_connections,
            max_header_bytes,
            max_request_body_bytes,
            resolver,
            resolver_addresses,
            resolver_queries,
            resolver_revalidate_on_connect: service.resolver.revalidate_on_connect,
            h3_upstream_connections: service
                .enabled_versions
                .contains(&ForwardHttpVersion::H3)
                .then_some(
                    max_connections
                        .clamp(1, crate::http3::upstream_plan::H3_UPSTREAM_MAX_CONNECTIONS),
                ),
        })
    }
}

fn compile_forward_auth(
    auth: Option<&ForwardProxyAuth>,
) -> Result<(Option<ForwardAuthBlueprint>, Option<HeaderValue>), ForwardPlanError> {
    match auth {
        Some(ForwardProxyAuth::BearerTokenFile { token_file_path }) => Ok((
            Some(ForwardAuthBlueprint::Bearer {
                token_file_path: token_file_path.clone(),
            }),
            Some(HeaderValue::from_static("Bearer")),
        )),
        Some(ForwardProxyAuth::BasicHtpasswdFile {
            htpasswd_file_path,
            realm,
            credential_ttl_ms,
            username_case_sensitive,
        }) => {
            let challenge =
                HeaderValue::from_str(&format!("Basic realm=\"{realm}\", charset=\"UTF-8\""))
                    .map_err(|_| ForwardPlanError::Authentication)?;
            Ok((
                Some(ForwardAuthBlueprint::Basic {
                    htpasswd_file_path: htpasswd_file_path.clone(),
                    realm: realm.clone(),
                    credential_ttl: credential_ttl_ms.map(Duration::from_millis),
                    username_case_sensitive: *username_case_sensitive,
                }),
                Some(challenge),
            ))
        }
        Some(ForwardProxyAuth::MutualTls { .. }) => Err(ForwardPlanError::Authentication),
        None => Ok((None, None)),
    }
}

fn compile_resolver_blueprint(policy: &ForwardResolverPolicy) -> ForwardResolverBlueprint {
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4thenIpv6;
    options.cache_size = policy.max_cache_entries;
    options.positive_min_ttl = Some(Duration::from_millis(policy.min_ttl_ms));
    options.positive_max_ttl = Some(Duration::from_millis(policy.max_ttl_ms));
    options.negative_min_ttl = Some(Duration::from_millis(policy.negative_ttl_ms));
    options.negative_max_ttl = Some(Duration::from_millis(policy.negative_ttl_ms));
    ForwardResolverBlueprint {
        nameservers: policy.nameservers.clone().into(),
        options,
    }
}

fn static_peer_plan(peer: &ForwardPeer) -> Result<StaticPeerPlan, ForwardPlanError> {
    if peer.host.is_empty() || peer.port == 0 {
        return Err(ForwardPlanError::Peer);
    }
    let host = peer
        .host
        .parse::<IpAddr>()
        .map_or_else(|_| Host::Dns(peer.host.clone()), Host::Ip);
    Ok(StaticPeerPlan {
        host,
        port: peer.port,
    })
}

fn destination_time_windows(ranges: &[ForwardTimeRange]) -> Result<Vec<TimeWindow>, RuleError> {
    ranges
        .iter()
        .map(|range| {
            let days = range.days.iter().fold(0_u8, |days, day| {
                days | match day {
                    ForwardWeekday::Monday => 1 << 0,
                    ForwardWeekday::Tuesday => 1 << 1,
                    ForwardWeekday::Wednesday => 1 << 2,
                    ForwardWeekday::Thursday => 1 << 3,
                    ForwardWeekday::Friday => 1 << 4,
                    ForwardWeekday::Saturday => 1 << 5,
                    ForwardWeekday::Sunday => 1 << 6,
                }
            });
            TimeWindow::new(
                days,
                parse_forward_time(&range.start).ok_or(RuleError::InvalidTimeWindow)?,
                parse_forward_time(&range.end).ok_or(RuleError::InvalidTimeWindow)?,
            )
        })
        .collect()
}

fn parse_forward_time(value: &str) -> Option<u16> {
    if value.len() != 5 || !value.is_ascii() || value.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let hour = value[..2].parse::<u16>().ok()?;
    let minute = value[3..].parse::<u16>().ok()?;
    if hour > 24 || minute > 59 || (hour == 24 && minute != 0) {
        return None;
    }
    Some(hour * 60 + minute)
}
