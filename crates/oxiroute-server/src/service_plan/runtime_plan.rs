#[allow(clippy::wildcard_imports)]
use super::*;

/// Immutable, resource-free decisions for one validated runtime generation.
pub struct RuntimePlan {
    pub max_connections: Option<u64>,
    pub rtmp_capabilities: RtmpCapabilities,
    pub rtmp_recording_supported: bool,
    pub topology: Arc<TopologySnapshot>,
    source: Arc<ValidatedConfig>,
    passive_policy: Option<PassiveFailurePolicy>,
}

pub(crate) struct GenerationAcquisition {
    l4_services: Option<Vec<Arc<L4ServicePlan>>>,
    forward_services: Option<Vec<Arc<ForwardHttp1ServicePlan>>>,
    http_services: Option<Vec<Arc<HttpServicePlan>>>,
    compiled_pools: Option<CompiledPools>,
    services: Option<Vec<ServiceSpec>>,
    health_supervisor: Option<HealthSupervisor>,
    pools: Option<Vec<Arc<RoundRobinPool>>>,
    tls: Option<PreparedTls>,
}

impl GenerationAcquisition {
    fn empty() -> Self {
        Self {
            l4_services: None,
            forward_services: None,
            http_services: None,
            compiled_pools: None,
            services: None,
            health_supervisor: None,
            pools: None,
            tls: None,
        }
    }

    pub(crate) fn services(&self) -> &[ServiceSpec] {
        self.services.as_deref().expect("uncommitted services")
    }

    pub(crate) fn pools(&self) -> &[Arc<RoundRobinPool>] {
        self.pools.as_deref().unwrap_or_else(|| {
            self.compiled_pools
                .as_ref()
                .expect("uncommitted pools")
                .ordered
                .as_slice()
        })
    }

    pub(crate) fn tls(&self) -> &PreparedTls {
        self.tls.as_ref().expect("uncommitted TLS")
    }

    pub(crate) fn commit(
        &mut self,
    ) -> (
        Vec<ServiceSpec>,
        Option<HealthSupervisor>,
        Vec<Arc<RoundRobinPool>>,
        PreparedTls,
    ) {
        self.l4_services.take();
        self.forward_services.take();
        self.http_services.take();
        self.compiled_pools.take();
        (
            self.services.take().expect("uncommitted services"),
            self.health_supervisor.take(),
            self.pools.take().expect("uncommitted pools"),
            self.tls.take().expect("uncommitted TLS"),
        )
    }
}

impl Drop for GenerationAcquisition {
    fn drop(&mut self) {
        self.services.take();
        self.l4_services.take();
        self.forward_services.take();
        self.http_services.take();
        self.health_supervisor.take();
        self.pools.take();
        self.compiled_pools.take();
        self.tls.take();
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum AcquisitionMode {
    Activate,
    Validate,
}

/// Compiles one immutable runtime generation including traffic and health services.
///
/// # Errors
///
/// Returns an error when a pool, route, reference, or health probe cannot be compiled.
///
/// ```compile_fail
/// use oxiroute_config::ConfigDraft;
///
/// let draft: ConfigDraft = todo!();
/// let _ = oxiroute_server::runtime_plan(&draft);
/// ```
pub fn runtime_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    runtime_plan_with_passive_failure_policy_internal(config, None)
}

/// Validates runtime acquisitions without retaining resources, then returns the immutable plan.
///
/// # Errors
///
/// Returns an error when any runtime resource cannot be acquired safely.
pub fn validate_runtime_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    let plan = runtime_plan(config)?;
    let acquired = validate_runtime_services(&plan)?;
    prepare_rtmp_service_specs(acquired.services(), RtmpPrepareMode::Validation)?;
    Ok(plan)
}

pub(super) fn prepare_rtmp_service_specs(
    services: &[ServiceSpec],
    mode: RtmpPrepareMode,
) -> Result<(), ServicePlanError> {
    let mut plans = Vec::new();
    for service in services {
        let ServiceKind::Rtmp(service) = &service.kind else {
            continue;
        };
        if !plans
            .iter()
            .any(|plan: &oxiroute_rtmp::RtmpServicePlan| plan.service_id() == service.service_id())
        {
            plans.push(service.value_plan());
        }
    }
    let listener_addresses = services.iter().filter_map(|service| match service.bind {
        ListenerBind::Socket { address, .. } | ListenerBind::Udp { address } => Some(address),
        ListenerBind::Unix { .. } => None,
    });
    let context = RtmpPrepareContext::new(mode, listener_addresses);
    PreparedRtmpRuntimeSet::prepare(plans, &context, Instant::now() + Duration::from_secs(5))
        .map(drop)
        .map_err(ServicePlanError::RtmpRuntimePreparation)
}

pub(crate) fn validation_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    runtime_plan_with_passive_failure_policy_internal(config, None)
}

/// Compiles one immutable runtime generation with an explicit passive endpoint policy.
///
/// # Errors
///
/// Returns an error when a pool, route, reference, or health probe cannot be compiled, including
/// when the passive policy exceeds its runtime bounds.
pub fn runtime_plan_with_passive_failure_policy(
    config: &ValidatedConfig,
    passive_policy: PassiveFailurePolicy,
) -> Result<RuntimePlan, ServicePlanError> {
    runtime_plan_with_passive_failure_policy_internal(config, Some(passive_policy))
}

fn runtime_plan_with_passive_failure_policy_internal(
    config: &ValidatedConfig,
    passive_policy: Option<PassiveFailurePolicy>,
) -> Result<RuntimePlan, ServicePlanError> {
    let blueprint = GenerationCompiler::compile(config)?;
    if let Some(passive_policy) = passive_policy {
        passive_policy
            .validate()
            .map_err(|source| ServicePlanError::Pool {
                pool: "passive-policy-override".into(),
                source,
            })?;
    }
    Ok(RuntimePlan {
        max_connections: blueprint.max_connections,
        rtmp_capabilities: blueprint.rtmp_capabilities,
        rtmp_recording_supported: blueprint.rtmp_recording_supported,
        topology: blueprint.topology,
        source: Arc::new(config.clone()),
        passive_policy,
    })
}

pub(crate) fn acquire_runtime_services(
    plan: &RuntimePlan,
) -> Result<GenerationAcquisition, ServicePlanError> {
    acquire_runtime_services_with_deadline(plan, None).map_err(|error| match error {
        RuntimeAcquisitionError::ServicePlan(error) => error,
        RuntimeAcquisitionError::PreparationTimedOut => {
            unreachable!("unbounded runtime acquisition cannot time out")
        }
    })
}

pub(crate) fn acquire_runtime_services_with_deadline(
    plan: &RuntimePlan,
    deadline: Option<Instant>,
) -> Result<GenerationAcquisition, RuntimeAcquisitionError> {
    acquire_runtime_services_with_mode(plan, AcquisitionMode::Activate, deadline)
}

pub(crate) fn validate_runtime_services(
    plan: &RuntimePlan,
) -> Result<GenerationAcquisition, ServicePlanError> {
    acquire_runtime_services_with_mode(plan, AcquisitionMode::Validate, None).map_err(|error| {
        match error {
            RuntimeAcquisitionError::ServicePlan(error) => error,
            RuntimeAcquisitionError::PreparationTimedOut => {
                unreachable!("runtime validation has no deadline")
            }
        }
    })
}

fn acquire_runtime_services_with_mode(
    plan: &RuntimePlan,
    mode: AcquisitionMode,
    deadline: Option<Instant>,
) -> Result<GenerationAcquisition, RuntimeAcquisitionError> {
    let blueprint = GenerationCompiler::compile(&plan.source)?;
    let mut acquired = GenerationAcquisition::empty();
    acquired.tls = Some(
        crate::tls::prepare_tls_blueprint(&blueprint.tls)
            .map_err(|source| ServicePlanError::Tls(Box::new(source)))?,
    );
    let pool_specs = blueprint.pool_specs?;
    acquired.compiled_pools = Some(compile_pools(
        &pool_specs,
        &blueprint.protected_addresses,
        plan.passive_policy,
    )?);
    let cache_specs = blueprint.cache_specs?;
    let http_service_specs = blueprint.http_service_specs?;
    acquired.http_services = Some(compile_http_services(
        &http_service_specs,
        &cache_specs,
        &acquired
            .compiled_pools
            .as_ref()
            .expect("acquired pools")
            .plans,
        mode,
        deadline,
    )?);
    acquired.forward_services = Some(compile_forward_proxy_services(
        &blueprint.forward_service_specs?,
        &cache_specs,
        mode,
        deadline,
    )?);
    let rtmp_services = compile_rtmp_services(&blueprint.rtmp_specs?, mode)?;
    acquired.l4_services = Some(compile_l4_services(
        &blueprint.l4_service_specs?,
        &acquired
            .compiled_pools
            .as_ref()
            .expect("acquired pools")
            .plans,
    ));
    acquired.services = Some(
        blueprint
            .listener_specs?
            .iter()
            .map(|listener| {
                compile_listener(
                    listener,
                    acquired.http_services.as_deref().expect("acquired HTTP"),
                    acquired
                        .forward_services
                        .as_deref()
                        .expect("acquired forward proxy"),
                    &rtmp_services,
                    acquired.l4_services.as_deref().expect("acquired L4"),
                    acquired.tls().profiles(),
                    &blueprint.tls,
                )
            })
            .collect(),
    );
    let pools = acquired.compiled_pools.take().expect("acquired pools");
    acquired.health_supervisor = (!pools.health_groups.is_empty() || !pools.canaries.is_empty())
        .then(|| HealthSupervisor::new(pools.health_groups, pools.canaries));
    acquired.pools = Some(pools.ordered);
    Ok(acquired)
}
