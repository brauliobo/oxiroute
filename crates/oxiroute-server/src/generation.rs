use std::{
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::ValidatedConfig;
use oxiroute_config_source::ConfigFormat;
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpAutoPushStatus, RtmpControlHandle, RtmpPrepareContext,
    RtmpPrepareMode, RtmpRuntimeSetError, RtmpServiceHandle, RtmpShutdown,
};
#[cfg(target_os = "linux")]
use oxiroute_supervision_unix::DescriptorSet;
use pingora::apps::{AcceptGate, AcceptGateClose, AcceptOwnership};
use serde::Serialize;

use crate::generation_resources::GenerationResources;
use crate::listener_inventory::{ListenerId, ListenerInventory};
use crate::rtmp_generation_runtime::{RtmpRetirement, RtmpRetirementRegistry};
#[cfg(test)]
use crate::service_plan::acquire_runtime_services;
use crate::{
    ListenerMetrics, ListenerReservations, MetricsError, ProcessRuntime, RuntimeMetrics,
    RuntimePlan, ServiceKind,
    config_coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
    runtime_plan,
    service_plan::validation_plan,
    service_plan::{
        RuntimeAcquisitionError, acquire_runtime_services_with_deadline, validate_runtime_services,
    },
};

pub(crate) const GENERATION_PREPARATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeReferenceKind {
    ForwardHttp1,
    Http1,
    Http2,
    WebSocket,
    Tcp,
    Rtmp,
    Udp,
    ForwardHttp3,
    Http3,
}

impl RuntimeReferenceKind {
    const fn index(self) -> usize {
        match self {
            Self::ForwardHttp1 => 0,
            Self::Http1 => 1,
            Self::Http2 => 2,
            Self::WebSocket => 3,
            Self::Tcp => 4,
            Self::Rtmp => 5,
            Self::Udp => 6,
            Self::ForwardHttp3 => 7,
            Self::Http3 => 8,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRevision {
    pub disk: AuthoredRevision,
    pub candidate: EffectiveRevision,
}

pub struct PreparedGeneration {
    config: Arc<ValidatedConfig>,
    inventory: ListenerInventory,
    metrics: RuntimeMetrics,
    resources: GenerationResources,
    revision: GenerationRevision,
}

enum ListenerSource<'a> {
    BindOrReuse {
        previous: Option<&'a ListenerReservations>,
        process: ProcessRuntime,
    },
    #[cfg(target_os = "linux")]
    Adopt {
        descriptors: DescriptorSet,
        process: ProcessRuntime,
    },
    Validate {
        previous: Option<&'a ListenerReservations>,
    },
}

#[expect(
    clippy::large_enum_variant,
    reason = "the private output transfers one complete prepared generation without another owner"
)]
enum PreparationOutput {
    Prepared(PreparedGeneration),
    Validated,
}

struct PreparedGenerationTransaction {
    #[cfg(test)]
    drop_probes: Vec<PreparedTransactionDropProbe>,
    reservations_precede_plan: bool,
    #[cfg(target_os = "linux")]
    descriptors: Option<DescriptorSet>,
    rtmp: Option<PreparedRtmpRuntimeSet>,
    listener_registration: Option<crate::monitoring::ListenerRegistrationTransaction>,
    reservations: Option<ListenerReservations>,
    acquired: Option<crate::service_plan::GenerationAcquisition>,
    plan: Option<RuntimePlan>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparedTransactionFault {
    Reservations,
    ListenerRegistration,
    RtmpPreparation,
}

#[cfg(test)]
std::thread_local! {
    static PREPARED_TRANSACTION_FAULT: std::cell::Cell<Option<PreparedTransactionFault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn with_prepared_transaction_fault<T>(
    fault: PreparedTransactionFault,
    run: impl FnOnce() -> T,
) -> T {
    PREPARED_TRANSACTION_FAULT.set(Some(fault));
    let result = run();
    PREPARED_TRANSACTION_FAULT.set(None);
    result
}

#[cfg(test)]
fn fail_after_prepared_transaction_stage(
    stage: PreparedTransactionFault,
) -> Result<(), GenerationError> {
    if PREPARED_TRANSACTION_FAULT.get() == Some(stage) {
        return Err(GenerationError::RuntimePrepare);
    }
    Ok(())
}

#[cfg(not(test))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "production and test builds share preparation boundary-fault call sites"
)]
fn fail_after_prepared_transaction_stage(_stage: ()) -> Result<(), GenerationError> {
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static PREPARED_TRANSACTION_ROLLBACK_TRACE: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl PreparedGenerationTransaction {
    fn new() -> Self {
        Self {
            #[cfg(test)]
            drop_probes: Vec::new(),
            reservations_precede_plan: false,
            #[cfg(target_os = "linux")]
            descriptors: None,
            rtmp: None,
            listener_registration: None,
            reservations: None,
            acquired: None,
            plan: None,
        }
    }

    fn acquired(&self) -> &crate::service_plan::GenerationAcquisition {
        self.acquired.as_ref().expect("provisional acquisition")
    }

    fn commit(mut self) -> GenerationResources {
        #[cfg(test)]
        for probe in &mut self.drop_probes {
            probe.armed = false;
        }
        GenerationResources::commit(
            self.plan.take().expect("provisional runtime plan"),
            self.acquired.take().expect("provisional acquisition"),
            self.reservations.take().expect("provisional reservations"),
            self.listener_registration
                .take()
                .expect("provisional listener registration"),
            self.rtmp.take().expect("provisional RTMP runtime"),
        )
    }
}

impl Drop for PreparedGenerationTransaction {
    fn drop(&mut self) {
        self.rtmp.take();
        #[cfg(test)]
        drop_prepared_transaction_probe(&mut self.drop_probes, "rtmp_prepared");
        #[cfg(test)]
        self.listener_registration.take();
        #[cfg(test)]
        drop_prepared_transaction_probe(&mut self.drop_probes, "listener_registration");
        if self.reservations_precede_plan {
            self.acquired.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "acquisition");
            self.plan.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "plan");
            self.reservations.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "reservations");
        } else {
            self.reservations.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "reservations");
            self.acquired.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "acquisition");
            self.plan.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "plan");
        }
        #[cfg(target_os = "linux")]
        self.descriptors.take();
    }
}

#[cfg(test)]
struct PreparedTransactionDropProbe {
    stage: &'static str,
    armed: bool,
}

#[cfg(test)]
impl PreparedTransactionDropProbe {
    const fn new(stage: &'static str) -> Self {
        Self { stage, armed: true }
    }
}

#[cfg(test)]
impl Drop for PreparedTransactionDropProbe {
    fn drop(&mut self) {
        if self.armed {
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().push(self.stage));
        }
    }
}

#[cfg(test)]
fn drop_prepared_transaction_probe(
    probes: &mut Vec<PreparedTransactionDropProbe>,
    stage: &'static str,
) {
    if probes.last().is_some_and(|probe| probe.stage == stage) {
        drop(probes.pop());
    }
}

impl PreparedGeneration {
    #[cfg(test)]
    fn prepare_rtmp(
        services: &[crate::ServiceSpec],
    ) -> Result<PreparedRtmpRuntimeSet, GenerationError> {
        Self::prepare_rtmp_with_deadline(services, RtmpPrepareMode::Activation, None)
    }

    fn prepare_rtmp_with_deadline(
        services: &[crate::ServiceSpec],
        mode: RtmpPrepareMode,
        deadline: Option<Instant>,
    ) -> Result<PreparedRtmpRuntimeSet, GenerationError> {
        let mut plans = Vec::new();
        for service in services {
            let ServiceKind::Rtmp(service) = &service.kind else {
                continue;
            };
            if !plans.iter().any(|plan: &oxiroute_rtmp::RtmpServicePlan| {
                plan.service_id() == service.service_id()
            }) {
                #[cfg(test)]
                if crate::service_plan::trace_staged_rtmp_prepare()
                    != crate::service_plan::RtmpRuntimeFault::None
                {
                    return Err(GenerationError::RuntimePrepare);
                }
                plans.push(service.value_plan());
            }
        }
        let listener_addresses = services.iter().filter_map(|service| match service.bind {
            oxiroute_config::ListenerBind::Socket { address }
            | oxiroute_config::ListenerBind::Udp { address } => Some(address),
            oxiroute_config::ListenerBind::Unix { .. } => None,
        });
        let context = RtmpPrepareContext::new(mode, listener_addresses);
        let deadline = deadline.unwrap_or_else(|| Instant::now() + GENERATION_PREPARATION_TIMEOUT);
        let prepared = PreparedRtmpRuntimeSet::prepare(plans, &context, deadline)
            .map_err(map_rtmp_runtime_set_error)?;
        Ok(prepared)
    }

    fn check_generation_inputs(
        config: &ValidatedConfig,
        tls: &crate::PreparedTls,
    ) -> Result<(), GenerationError> {
        let draft = config.as_draft();
        crate::stats::preflight_admin_token(
            draft
                .stats
                .as_ref()
                .and_then(|stats| stats.admin_token_file.as_deref()),
        )?;
        if draft.management.is_some() {
            let token_file = std::env::var_os("OXIROUTE_MANAGEMENT_TOKEN_FILE")
                .map(std::path::PathBuf::from)
                .ok_or(GenerationError::ManagementToken)?;
            crate::rtmp_api::preflight_management_token(&token_file)
                .map_err(|_| GenerationError::ManagementToken)?;
        }
        if let Some(ui_dir) = draft
            .management
            .as_ref()
            .and_then(|management| management.ui_dir.as_deref())
        {
            crate::rtmp_api::UiAssets::load(ui_dir).map_err(|_| GenerationError::RuntimePrepare)?;
        }
        tls.check_certbot_watcher(crate::CertbotWatcherConfig::default())
            .map_err(|_| GenerationError::RuntimePrepare)?;
        tls.check_file_watcher(crate::FileWatcherConfig::default())
            .map_err(|_| GenerationError::RuntimePrepare)?;
        Ok(())
    }

    fn prepare_from(
        document: ResolvedConfigDocument,
        source: ListenerSource<'_>,
    ) -> Result<PreparationOutput, GenerationError> {
        Self::prepare_from_with_deadline(document, source, None)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "source-specific acquisition and the common transactional tail remain one auditable state machine"
    )]
    fn prepare_from_with_deadline(
        document: ResolvedConfigDocument,
        source: ListenerSource<'_>,
        deadline: Option<Instant>,
    ) -> Result<PreparationOutput, GenerationError> {
        let revision = GenerationRevision {
            disk: document.authored_revision,
            candidate: document.effective_revision,
        };
        let config = Arc::new(document.validated_config);
        let inventory = ListenerInventory::compile(&config);
        inventory.validate_public_display_names()?;
        let mut transaction = PreparedGenerationTransaction::new();
        let process = match source {
            ListenerSource::BindOrReuse { previous, process } => {
                transaction.plan = Some(
                    runtime_plan(&config)
                        .map_err(|source| GenerationError::Plan(Box::new(source)))?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("plan"));
                transaction.acquired = Some(
                    acquire_runtime_services_with_deadline(
                        transaction.plan.as_ref().expect("provisional runtime plan"),
                        deadline,
                    )
                    .map_err(GenerationError::from)?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("acquisition"));
                transaction.reservations = Some(ListenerReservations::prepare(&config, previous)?);
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("reservations"));
                Some(process)
            }
            #[cfg(target_os = "linux")]
            ListenerSource::Adopt {
                descriptors,
                process,
            } => {
                transaction.reservations_precede_plan = true;
                transaction.descriptors = Some(descriptors);
                transaction.reservations = Some(ListenerReservations::adopt(
                    &config,
                    transaction
                        .descriptors
                        .take()
                        .expect("provisional descriptor set"),
                )?);
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("reservations"));
                transaction.plan = Some(
                    runtime_plan(&config)
                        .map_err(|source| GenerationError::Plan(Box::new(source)))?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("plan"));
                transaction.acquired = Some(
                    acquire_runtime_services_with_deadline(
                        transaction.plan.as_ref().expect("provisional runtime plan"),
                        deadline,
                    )
                    .map_err(GenerationError::from)?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("acquisition"));
                Some(process)
            }
            ListenerSource::Validate { previous } => {
                transaction.plan = Some(
                    validation_plan(&config)
                        .map_err(|source| GenerationError::Plan(Box::new(source)))?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("plan"));
                transaction.acquired = Some(
                    validate_runtime_services(
                        transaction
                            .plan
                            .as_ref()
                            .expect("provisional validation plan"),
                    )
                    .map_err(|source| GenerationError::Plan(Box::new(source)))?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("acquisition"));
                transaction.reservations = Some(ListenerReservations::prepare_for_validation(
                    &config, previous,
                )?);
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("reservations"));
                None
            }
        };
        #[cfg(test)]
        fail_after_prepared_transaction_stage(PreparedTransactionFault::Reservations)?;
        #[cfg(not(test))]
        fail_after_prepared_transaction_stage(())?;
        Self::check_generation_inputs(&config, transaction.acquired().tls())?;
        let metrics = if let Some(process) = process {
            let metrics = RuntimeMetrics::for_process(process);
            transaction.listener_registration =
                Some(metrics.register_inventory(inventory.entries())?);
            #[cfg(test)]
            transaction
                .drop_probes
                .push(PreparedTransactionDropProbe::new("listener_registration"));
            #[cfg(test)]
            fail_after_prepared_transaction_stage(PreparedTransactionFault::ListenerRegistration)?;
            #[cfg(not(test))]
            fail_after_prepared_transaction_stage(())?;
            metrics.register_upstream_pools(transaction.acquired().pools().iter().cloned())?;
            Some(metrics)
        } else {
            None
        };
        transaction.rtmp = Some(Self::prepare_rtmp_with_deadline(
            transaction.acquired().services(),
            if metrics.is_some() {
                RtmpPrepareMode::Activation
            } else {
                RtmpPrepareMode::Validation
            },
            deadline,
        )?);
        #[cfg(test)]
        transaction
            .drop_probes
            .push(PreparedTransactionDropProbe::new("rtmp_prepared"));
        #[cfg(test)]
        fail_after_prepared_transaction_stage(PreparedTransactionFault::RtmpPreparation)?;
        #[cfg(not(test))]
        fail_after_prepared_transaction_stage(())?;
        let Some(metrics) = metrics else {
            return Ok(PreparationOutput::Validated);
        };
        metrics.set_rtmp_recording_supported(
            transaction
                .plan
                .as_ref()
                .expect("provisional runtime plan")
                .rtmp_recording_supported,
        );
        Ok(PreparationOutput::Prepared(Self {
            config,
            inventory,
            metrics,
            resources: transaction.commit(),
            revision,
        }))
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        &self.revision
    }
}

pub struct RuntimeGeneration {
    accept_gate: AcceptGate,
    config: Arc<ValidatedConfig>,
    drain: (Mutex<()>, Condvar),
    metrics: RuntimeMetrics,
    inventory: ListenerInventory,
    resources: GenerationResources,
    references: [AtomicU64; 9],
    mutations: AtomicU64,
    revision: GenerationRevision,
    runtime_lifecycle: AtomicU8,
    runtime_failed: AtomicBool,
}

const RUNTIME_PREPARED: u8 = 0;
const RUNTIME_START_CLAIMED: u8 = 1;
const RUNTIME_STARTED: u8 = 2;
const RUNTIME_CANCELLED: u8 = 3;

impl RuntimeGeneration {
    fn activate(prepared: PreparedGeneration) -> Self {
        Self {
            accept_gate: AcceptGate::closed(),
            config: prepared.config,
            drain: (Mutex::new(()), Condvar::new()),
            metrics: prepared.metrics,
            inventory: prepared.inventory,
            resources: prepared.resources,
            references: std::array::from_fn(|_| AtomicU64::new(0)),
            mutations: AtomicU64::new(0),
            revision: prepared.revision,
            runtime_lifecycle: AtomicU8::new(RUNTIME_PREPARED),
            runtime_failed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub fn config(&self) -> &Arc<ValidatedConfig> {
        &self.config
    }

    #[must_use]
    pub const fn plan(&self) -> &RuntimePlan {
        self.resources.plan()
    }

    #[must_use]
    pub fn services(&self) -> &[crate::ServiceSpec] {
        self.resources.services()
    }

    #[must_use]
    pub fn health_supervisor(&self) -> Option<crate::HealthSupervisor> {
        self.resources.health_supervisor()
    }

    #[must_use]
    pub fn pools(&self) -> &[Arc<crate::RoundRobinPool>] {
        self.resources.pools()
    }

    #[must_use]
    pub const fn tls(&self) -> &crate::PreparedTls {
        self.resources.tls()
    }

    #[must_use]
    pub fn rtmp_vod_catalog(&self) -> Arc<oxiroute_rtmp::VodCatalog> {
        self.resources.rtmp_vod_catalog()
    }

    #[must_use]
    pub fn rtmp_media_catalog(&self) -> Arc<oxiroute_rtmp::MediaCatalog> {
        self.resources.rtmp_media_catalog()
    }

    #[must_use]
    pub const fn metrics(&self) -> &RuntimeMetrics {
        &self.metrics
    }

    fn listener_metrics(&self, id: &ListenerId) -> Result<ListenerMetrics, MetricsError> {
        self.metrics
            .inventory_listener(id)?
            .ok_or_else(|| MetricsError::ListenerNotFound(format!("{id:?}")))
    }

    /// Returns the metrics handle for one exact traffic listener identity.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state is unavailable or the listener is not registered.
    pub fn traffic_listener_metrics(&self, name: &str) -> Result<ListenerMetrics, MetricsError> {
        self.listener_metrics(&ListenerId::Traffic(name.to_owned()))
    }

    /// Returns the metrics handle for the management listener.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state is unavailable or the listener is not registered.
    pub fn management_listener_metrics(&self) -> Result<ListenerMetrics, MetricsError> {
        self.listener_metrics(&ListenerId::Management)
    }

    /// Returns the metrics handle for one exact statistics listener index.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state is unavailable or the listener is not registered.
    pub fn stats_listener_metrics(&self, index: usize) -> Result<ListenerMetrics, MetricsError> {
        self.listener_metrics(&ListenerId::Stats(index))
    }

    /// Returns the metrics handle for one exact statistics-page listener index.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state is unavailable or the listener is not registered.
    pub fn stats_page_listener_metrics(
        &self,
        index: usize,
    ) -> Result<ListenerMetrics, MetricsError> {
        self.listener_metrics(&ListenerId::StatsPage(index))
    }

    #[must_use]
    pub const fn reservations(&self) -> &ListenerReservations {
        self.resources.reservations()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn expected_runtime_listener_count(&self) -> usize {
        self.inventory.complete_listener_count()
    }

    pub(crate) fn listener_restart_required(
        &self,
        mode: crate::RuntimeMode,
        candidate: &ValidatedConfig,
    ) -> bool {
        self.inventory
            .restart_required(mode, &ListenerInventory::compile(candidate))
    }

    pub(crate) fn listener_restart_reason(
        &self,
        mode: crate::RuntimeMode,
        candidate: &ValidatedConfig,
    ) -> Option<crate::listener_inventory::ListenerRestartReason> {
        self.inventory
            .restart_reason(mode, &ListenerInventory::compile(candidate))
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        &self.revision
    }

    #[must_use]
    pub fn rtmp_control(&self) -> RtmpControlHandle {
        self.resources.rtmp_control()
    }

    #[must_use]
    pub fn rtmp_service(&self, service: &str) -> Option<RtmpServiceHandle> {
        self.resources.rtmp_service(service)
    }

    #[must_use]
    pub fn rtmp_auto_push_status(&self) -> RtmpAutoPushStatus {
        self.resources.rtmp_auto_push_status()
    }

    pub fn initiate_rtmp_shutdown(&self, deadline: Instant) -> RtmpShutdown {
        self.resources.initiate_rtmp_shutdown(deadline)
    }

    fn rtmp_retirement(&self) -> RtmpRetirement {
        self.resources.rtmp_retirement()
    }

    pub fn rtmp_shutdown(&self) -> RtmpShutdown {
        self.resources.rtmp_shutdown()
    }

    fn claim_runtime_start(&self) -> bool {
        self.runtime_lifecycle
            .compare_exchange(
                RUNTIME_PREPARED,
                RUNTIME_START_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn start_rtmp(&self) -> Result<(), GenerationError> {
        self.resources.start_rtmp()
    }

    #[must_use]
    pub fn mark_runtime_started(&self) -> bool {
        self.runtime_lifecycle
            .compare_exchange(
                RUNTIME_START_CLAIMED,
                RUNTIME_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[must_use]
    pub fn runtime_started(&self) -> bool {
        self.runtime_lifecycle.load(Ordering::Acquire) == RUNTIME_STARTED
    }

    pub fn mark_runtime_failed(&self) {
        self.runtime_failed.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn runtime_failed(&self) -> bool {
        self.runtime_failed.load(Ordering::Acquire)
    }

    fn cancel_runtime_start(&self) {
        self.runtime_lifecycle
            .store(RUNTIME_CANCELLED, Ordering::Release);
        self.runtime_failed.store(true, Ordering::Release);
    }

    pub fn begin_reference(
        self: &Arc<Self>,
        kind: RuntimeReferenceKind,
    ) -> Option<GenerationReference> {
        if !self.accept_gate.accepting() {
            return None;
        }
        let counter = &self.references[kind.index()];
        counter.fetch_add(1, Ordering::AcqRel);
        if !self.accept_gate.accepting() {
            counter.fetch_sub(1, Ordering::AcqRel);
            self.drain.1.notify_all();
            return None;
        }
        Some(GenerationReference {
            generation: Arc::clone(self),
            kind,
        })
    }

    #[must_use]
    pub fn begin_owned_reference(
        self: &Arc<Self>,
        kind: RuntimeReferenceKind,
    ) -> GenerationReference {
        self.references[kind.index()].fetch_add(1, Ordering::AcqRel);
        GenerationReference {
            generation: Arc::clone(self),
            kind,
        }
    }

    pub fn begin_admission(self: &Arc<Self>) -> Option<GenerationAdmission> {
        self.accept_gate
            .claim()
            .map(|ownership| GenerationAdmission {
                generation: Arc::clone(self),
                _ownership: ownership,
            })
    }

    #[must_use]
    pub fn accept_gate(&self) -> AcceptGate {
        self.accept_gate.clone()
    }

    pub fn stop_accepting(&self) {
        let _ = self.accept_gate.close_and_wait(Duration::ZERO);
    }

    fn start_accepting(&self) {
        self.metrics.activate_limits(self.plan().max_connections);
        self.accept_gate.enable();
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        self.accept_gate.accepting()
    }

    #[must_use]
    pub fn active_references(&self, kind: RuntimeReferenceKind) -> u64 {
        self.references[kind.index()].load(Ordering::Acquire)
    }

    #[must_use]
    pub fn drained(&self) -> bool {
        self.references
            .iter()
            .all(|counter| counter.load(Ordering::Acquire) == 0)
    }

    #[must_use]
    pub fn drain(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        if !self.accept_gate.close_and_wait(timeout) {
            return false;
        }
        let timeout = timeout.saturating_sub(started.elapsed());
        let deadline = Instant::now() + timeout;
        let mut lock = self
            .drain
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if self.drained() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, wait) = self
                .drain
                .1
                .wait_timeout(lock, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock = next;
            if wait.timed_out() {
                return self.drained();
            }
        }
    }
}

pub struct GenerationAdmission {
    generation: Arc<RuntimeGeneration>,
    _ownership: AcceptOwnership,
}

impl GenerationAdmission {
    #[must_use]
    pub fn accepting(&self) -> bool {
        self.generation.accepting()
    }
}

pub struct GenerationReference {
    generation: Arc<RuntimeGeneration>,
    kind: RuntimeReferenceKind,
}

pub struct GenerationMutation {
    generation: Arc<RuntimeGeneration>,
}

impl GenerationMutation {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        &self.generation
    }
}

impl Drop for GenerationMutation {
    fn drop(&mut self) {
        self.generation.mutations.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub struct GenerationCandidate {
    generation: Arc<RuntimeGeneration>,
    id: u64,
}

impl GenerationCandidate {
    #[must_use]
    pub fn generation(&self) -> &Arc<RuntimeGeneration> {
        &self.generation
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        self.generation.revision()
    }
}

impl Drop for GenerationReference {
    fn drop(&mut self) {
        self.generation.references[self.kind.index()].fetch_sub(1, Ordering::AcqRel);
        self.generation.drain.1.notify_all();
    }
}

#[derive(Default)]
struct GenerationState {
    active: Option<Arc<RuntimeGeneration>>,
    candidate: Option<GenerationCandidate>,
    disk_revision: Option<AuthoredRevision>,
    last_failure: Option<&'static str>,
    previous: Option<Arc<RuntimeGeneration>>,
    quarantined_revision: Option<EffectiveRevision>,
    shutdown_generations: Vec<Arc<RuntimeGeneration>>,
    shutting_down: bool,
    starting_candidate: Option<CandidateStartReservation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateStartPhase {
    Starting,
    RuntimeOwned,
    Activating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CandidateStartReservation {
    candidate_id: u64,
    phase: CandidateStartPhase,
    token: u64,
}

struct ActivationReservation {
    previous: Option<Arc<RuntimeGeneration>>,
    previous_revision: Option<EffectiveRevision>,
    runtime_owned: bool,
    token: u64,
}

#[derive(Default)]
struct GenerationCounters {
    activations: AtomicU64,
    failures: AtomicU64,
    prepares: AtomicU64,
    rollbacks: AtomicU64,
}

#[derive(Default)]
struct OperationGate {
    available: Condvar,
    held: Mutex<bool>,
}

impl OperationGate {
    fn acquire(self: &Arc<Self>) -> OperationAuthority {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *held {
            held = self
                .available
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *held = true;
        OperationAuthority {
            gate: Arc::clone(self),
        }
    }

    fn acquire_until(self: &Arc<Self>, deadline: Instant) -> Option<OperationAuthority> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if !*held {
                *held = true;
                return Some(OperationAuthority {
                    gate: Arc::clone(self),
                });
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, _) = self
                .available
                .wait_timeout(held, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held = next;
        }
    }
}

struct OperationAuthority {
    gate: Arc<OperationGate>,
}

impl Drop for OperationAuthority {
    fn drop(&mut self) {
        let mut held = self
            .gate
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = false;
        drop(held);
        self.gate.available.notify_one();
    }
}

#[derive(Clone)]
pub struct GenerationManager {
    #[cfg(test)]
    activation_hook: Arc<Mutex<Option<ActivationHook>>>,
    counters: Arc<GenerationCounters>,
    rtmp_retirements: Arc<Mutex<RtmpRetirementRegistry>>,
    next_candidate_id: Arc<AtomicU64>,
    next_reservation_token: Arc<AtomicU64>,
    operations: Arc<OperationGate>,
    process: ProcessRuntime,
    state: Arc<Mutex<GenerationState>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationHookPoint {
    Reserved,
    GateClosed,
}

#[cfg(test)]
type ActivationHook = Arc<dyn Fn(ActivationHookPoint) + Send + Sync>;

pub struct GenerationStartup {
    candidate: GenerationCandidate,
    manager: GenerationManager,
    reservation_token: u64,
    completed: bool,
}

impl GenerationStartup {
    /// Claims the single permitted runtime start for this candidate.
    ///
    /// # Errors
    ///
    /// Returns an error if the startup reservation or candidate is no longer current.
    pub fn claim_runtime_start(&mut self) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.manager
            .claim_runtime_start(&self.candidate, self.reservation_token)
    }

    pub fn activate(self) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.activate_with_timeout(Duration::from_secs(5))
    }

    fn activate_with_timeout(
        mut self,
        quiesce_timeout: Duration,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let activated = self.manager.activate_inner(
            &self.candidate,
            Some(self.reservation_token),
            quiesce_timeout,
        );
        self.completed = true;
        activated
    }
}

impl Drop for GenerationStartup {
    fn drop(&mut self) {
        if !self.completed {
            self.manager
                .cancel_startup(&self.candidate, self.reservation_token);
        }
    }
}

impl Default for GenerationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GenerationManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            activation_hook: Arc::new(Mutex::new(None)),
            counters: Arc::new(GenerationCounters::default()),
            rtmp_retirements: Arc::new(Mutex::new(RtmpRetirementRegistry::default())),
            next_candidate_id: Arc::new(AtomicU64::new(0)),
            next_reservation_token: Arc::new(AtomicU64::new(0)),
            operations: Arc::new(OperationGate::default()),
            process: ProcessRuntime::new(None),
            state: Arc::new(Mutex::new(GenerationState::default())),
        }
    }

    /// Creates a generation manager for a worker using authenticated listener adoption.
    #[must_use]
    pub fn new_supervised() -> Self {
        let mut manager = Self::new();
        manager.process = ProcessRuntime::supervised(None);
        manager
    }

    fn acquire_preparation_operation(
        &self,
        deadline: Option<Instant>,
        disk_revision: &AuthoredRevision,
        candidate_revision: &EffectiveRevision,
    ) -> Result<OperationAuthority, GenerationError> {
        let operation = match deadline {
            Some(deadline) => self.operations.acquire_until(deadline),
            None => Some(self.operations.acquire()),
        };
        let Some(operation) = operation else {
            let error = GenerationError::PreparationTimedOut;
            crate::operational_event::emit(
                "generation_prepare",
                "rejected",
                Some(candidate_revision),
            );
            self.counters.failures.fetch_add(1, Ordering::Relaxed);
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.disk_revision = Some(disk_revision.clone());
            state.last_failure = Some(error.code());
            return Err(error);
        };
        Ok(operation)
    }

    /// Fully prepares a candidate without writing the canonical configuration or publishing it.
    ///
    /// # Errors
    ///
    /// Returns a redacted preparation error. A failed candidate is released and active state is
    /// unchanged.
    pub fn prepare(
        &self,
        document: ResolvedConfigDocument,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_with_deadline_internal(document, None)
    }

    #[doc(hidden)]
    pub fn prepare_with_deadline(
        &self,
        document: ResolvedConfigDocument,
        deadline: Instant,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_with_deadline_internal(document, Some(deadline))
    }

    fn prepare_with_deadline_internal(
        &self,
        document: ResolvedConfigDocument,
        deadline: Option<Instant>,
    ) -> Result<GenerationCandidate, GenerationError> {
        let disk_revision = document.authored_revision.clone();
        let candidate_revision = document.effective_revision.clone();
        let _operation =
            self.acquire_preparation_operation(deadline, &disk_revision, &candidate_revision)?;
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutting_down
        {
            return Err(GenerationError::MutationInProgress);
        }
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|generation| generation.reservations().clone());
        let prepared = PreparedGeneration::prepare_from_with_deadline(
            document,
            ListenerSource::BindOrReuse {
                previous: previous.as_ref(),
                process: self.process.clone(),
            },
            deadline,
        )
        .and_then(|output| {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                drop(output);
                Err(GenerationError::PreparationTimedOut)
            } else {
                Ok(output)
            }
        })
        .map(|output| match output {
            PreparationOutput::Prepared(prepared) => {
                Arc::new(RuntimeGeneration::activate(prepared))
            }
            PreparationOutput::Validated => unreachable!("runtime preparation returned validation"),
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = Some(disk_revision.clone());
        match prepared {
            Ok(_generation) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                let error = GenerationError::PreparationTimedOut;
                crate::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                Err(error)
            }
            Ok(generation) => {
                let candidate = GenerationCandidate {
                    generation,
                    id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
                };
                crate::operational_event::emit(
                    "generation_prepare",
                    "prepared",
                    Some(&candidate.revision().candidate),
                );
                self.counters.prepares.fetch_add(1, Ordering::Relaxed);
                state.candidate = Some(candidate.clone());
                state.quarantined_revision = None;
                state.last_failure = None;
                Ok(candidate)
            }
            Err(error) => {
                crate::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                if !matches!(error, GenerationError::PreparationTimedOut) {
                    state.quarantined_revision = Some(candidate_revision);
                }
                state.last_failure = Some(error.code());
                Err(error)
            }
        }
    }

    /// Fully prepares a candidate from listener descriptors exactly adopted for its config.
    ///
    /// # Errors
    ///
    /// Returns a redacted preparation error. A failed candidate is released and active state is
    /// unchanged.
    #[cfg(target_os = "linux")]
    pub fn prepare_adopted(
        &self,
        document: ResolvedConfigDocument,
        descriptors: DescriptorSet,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_adopted_with_deadline_internal(document, descriptors, None)
    }

    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    pub fn prepare_adopted_with_deadline(
        &self,
        document: ResolvedConfigDocument,
        descriptors: DescriptorSet,
        deadline: Instant,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.prepare_adopted_with_deadline_internal(document, descriptors, Some(deadline))
    }

    #[cfg(target_os = "linux")]
    fn prepare_adopted_with_deadline_internal(
        &self,
        document: ResolvedConfigDocument,
        descriptors: DescriptorSet,
        deadline: Option<Instant>,
    ) -> Result<GenerationCandidate, GenerationError> {
        let disk_revision = document.authored_revision.clone();
        let candidate_revision = document.effective_revision.clone();
        let _operation =
            self.acquire_preparation_operation(deadline, &disk_revision, &candidate_revision)?;
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutting_down
        {
            return Err(GenerationError::MutationInProgress);
        }
        let prepared = PreparedGeneration::prepare_from_with_deadline(
            document,
            ListenerSource::Adopt {
                descriptors,
                process: self.process.clone(),
            },
            deadline,
        )
        .and_then(|output| {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                drop(output);
                Err(GenerationError::PreparationTimedOut)
            } else {
                Ok(output)
            }
        })
        .map(|output| match output {
            PreparationOutput::Prepared(prepared) => {
                Arc::new(RuntimeGeneration::activate(prepared))
            }
            PreparationOutput::Validated => unreachable!("runtime preparation returned validation"),
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = Some(disk_revision.clone());
        match prepared {
            Ok(_generation) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                let error = GenerationError::PreparationTimedOut;
                crate::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                Err(error)
            }
            Ok(generation) => {
                let candidate = GenerationCandidate {
                    generation,
                    id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
                };
                crate::operational_event::emit(
                    "generation_prepare",
                    "prepared",
                    Some(&candidate.revision().candidate),
                );
                self.counters.prepares.fetch_add(1, Ordering::Relaxed);
                state.candidate = Some(candidate.clone());
                state.quarantined_revision = None;
                state.last_failure = None;
                Ok(candidate)
            }
            Err(error) => {
                crate::operational_event::emit(
                    "generation_prepare",
                    "rejected",
                    Some(&candidate_revision),
                );
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                if !matches!(error, GenerationError::PreparationTimedOut) {
                    state.quarantined_revision = Some(candidate_revision);
                }
                state.last_failure = Some(error.code());
                Err(error)
            }
        }
    }

    /// Preflights RTMP resources and performs temporary listener bind probes without publishing.
    ///
    /// # Errors
    ///
    /// Returns redacted preflight or listener-probe errors. RTMP preflight does not acquire runtime
    /// ownership; listener probes temporarily bind sockets and release them before returning.
    /// Active and pending state are unchanged.
    pub fn validate_candidate(
        &self,
        document: ResolvedConfigDocument,
    ) -> Result<(), GenerationError> {
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|generation| generation.reservations().clone());
        match PreparedGeneration::prepare_from(
            document,
            ListenerSource::Validate {
                previous: previous.as_ref(),
            },
        )? {
            PreparationOutput::Validated => Ok(()),
            PreparationOutput::Prepared(_) => {
                unreachable!("validation preparation returned a runtime generation")
            }
        }
    }

    /// Atomically publishes the prepared candidate and stops new references to the old generation.
    ///
    /// # Errors
    ///
    /// Returns an error when no complete candidate is prepared.
    #[cfg(test)]
    pub(crate) fn activate(
        &self,
        candidate: &GenerationCandidate,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        self.activate_inner(candidate, None, Duration::from_secs(5))
    }

    fn activate_inner(
        &self,
        candidate: &GenerationCandidate,
        startup_token: Option<u64>,
        quiesce_timeout: Duration,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let reservation = self.reserve_activation(candidate, startup_token)?;
        #[cfg(test)]
        self.run_activation_hook(ActivationHookPoint::Reserved);
        let previous_close = reservation
            .previous
            .as_ref()
            .map(|previous| previous.accept_gate.close());
        #[cfg(test)]
        self.run_activation_hook(ActivationHookPoint::GateClosed);
        let quiesced = previous_close
            .as_ref()
            .is_none_or(|close| close.wait(quiesce_timeout));
        self.publish_reserved_candidate(candidate, &reservation, previous_close, quiesced)
    }

    fn reserve_activation(
        &self,
        candidate: &GenerationCandidate,
        startup_token: Option<u64>,
    ) -> Result<ActivationReservation, GenerationError> {
        let _operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            if let Some(token) = startup_token {
                self.cancel_startup_locked(&mut state, candidate, token);
            }
            return Err(GenerationError::MutationInProgress);
        }
        let (token, runtime_owned) = if let Some(token) = startup_token {
            let Some(starting) = state.starting_candidate else {
                return Err(GenerationError::CandidateSuperseded);
            };
            if starting.candidate_id != candidate.id || starting.token != token {
                return Err(GenerationError::CandidateSuperseded);
            }
            if starting.phase == CandidateStartPhase::Starting {
                state.starting_candidate = None;
                return Err(GenerationError::RuntimePrepare);
            }
            if starting.phase != CandidateStartPhase::RuntimeOwned {
                return Err(GenerationError::CandidateSuperseded);
            }
            if !candidate_matches(state.candidate.as_ref(), candidate) {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
                return Err(GenerationError::CandidateSuperseded);
            }
            (token, true)
        } else {
            if !candidate_matches(state.candidate.as_ref(), candidate) {
                return Err(GenerationError::CandidateSuperseded);
            }
            if state.starting_candidate.is_some() {
                return Err(GenerationError::MutationInProgress);
            }
            (self.next_reservation_token(), false)
        };
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            if runtime_owned {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
                self.quarantine_locked(&mut state, candidate, "runtime_start");
            }
            return Err(GenerationError::MutationInProgress);
        }
        if candidate.generation.runtime_failed()
            || (runtime_owned && !candidate.generation.runtime_started())
        {
            if runtime_owned {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
            }
            self.quarantine_locked(&mut state, candidate, "runtime_start");
            return Err(GenerationError::RuntimePrepare);
        }
        state.starting_candidate = Some(CandidateStartReservation {
            candidate_id: candidate.id,
            phase: CandidateStartPhase::Activating,
            token,
        });
        let previous = state.active.clone();
        Ok(ActivationReservation {
            previous_revision: previous
                .as_ref()
                .map(|generation| generation.revision.candidate.clone()),
            previous,
            runtime_owned,
            token,
        })
    }

    fn publish_reserved_candidate(
        &self,
        candidate: &GenerationCandidate,
        reservation: &ActivationReservation,
        previous_close: Option<AcceptGateClose>,
        quiesced: bool,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owns_reservation = state.starting_candidate
            == Some(CandidateStartReservation {
                candidate_id: candidate.id,
                phase: CandidateStartPhase::Activating,
                token: reservation.token,
            });
        let candidate_current = candidate_matches(state.candidate.as_ref(), candidate);
        let previous_current = active_matches_reservation(&state, reservation);
        let close_current = previous_close
            .as_ref()
            .is_none_or(AcceptGateClose::is_current);
        let failure = if state.shutting_down {
            Some(GenerationError::MutationInProgress)
        } else if !owns_reservation || !candidate_current || !previous_current {
            Some(GenerationError::CandidateSuperseded)
        } else if candidate.generation.runtime_failed() {
            Some(GenerationError::RuntimePrepare)
        } else if !quiesced || !close_current {
            Some(GenerationError::AcceptorQuiesce)
        } else if state
            .active
            .as_ref()
            .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            Some(GenerationError::MutationInProgress)
        } else {
            None
        };
        if let Some(error) = failure {
            if previous_current
                && !state.shutting_down
                && let Some(close) = previous_close
            {
                let _ = close.reopen();
            }
            if owns_reservation {
                state.starting_candidate = None;
                if reservation.runtime_owned {
                    candidate.generation.cancel_runtime_start();
                    if candidate_current {
                        let reason = if matches!(error, GenerationError::RuntimePrepare) {
                            "runtime_start"
                        } else {
                            error.code()
                        };
                        self.quarantine_locked(&mut state, candidate, reason);
                    }
                }
            }
            return Err(error);
        }

        state.candidate = None;
        state.starting_candidate = None;
        let active = Arc::clone(&candidate.generation);
        if let Some(previous) = state.active.replace(Arc::clone(&active)) {
            let evicted = state.previous.replace(previous);
            if let Some(evicted) = &evicted {
                let mut retirements = self
                    .rtmp_retirements
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                retirements.retire(evicted.rtmp_retirement());
            }
        }
        active.start_accepting();
        state.last_failure = None;
        self.counters.activations.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_activate",
            "activated",
            Some(&active.revision.candidate),
        );
        drop(state);
        drop(operation);
        Ok(active)
    }

    fn next_reservation_token(&self) -> u64 {
        self.next_reservation_token
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    #[cfg(test)]
    fn set_activation_hook(&self, hook: ActivationHook) {
        *self
            .activation_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    fn run_activation_hook(&self, point: ActivationHookPoint) {
        let hook = self
            .activation_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }

    fn claim_runtime_start(
        &self,
        candidate: &GenerationCandidate,
        reservation_token: u64,
    ) -> Result<Arc<RuntimeGeneration>, GenerationError> {
        let _operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            return Err(GenerationError::MutationInProgress);
        }
        let expected = CandidateStartReservation {
            candidate_id: candidate.id,
            phase: CandidateStartPhase::Starting,
            token: reservation_token,
        };
        if state.starting_candidate != Some(expected)
            || !candidate_matches(state.candidate.as_ref(), candidate)
        {
            clear_starting_reservation(
                &mut state,
                candidate.id,
                reservation_token,
                CandidateStartPhase::Starting,
            );
            return Err(GenerationError::CandidateSuperseded);
        }
        if !candidate.generation.claim_runtime_start() {
            state.starting_candidate = None;
            self.quarantine_locked(&mut state, candidate, "runtime_start");
            return Err(GenerationError::RuntimePrepare);
        }
        if candidate.generation.start_rtmp().is_err() {
            candidate.generation.cancel_runtime_start();
            state.starting_candidate = None;
            self.quarantine_locked(&mut state, candidate, "runtime_start");
            return Err(GenerationError::RuntimePrepare);
        }
        state.starting_candidate = Some(CandidateStartReservation {
            candidate_id: candidate.id,
            phase: CandidateStartPhase::RuntimeOwned,
            token: reservation_token,
        });
        Ok(Arc::clone(&candidate.generation))
    }

    /// Reserves publication before any candidate runtime is started.
    ///
    /// # Errors
    ///
    /// Returns an error when the candidate was superseded, another startup is reserved, or an
    /// active-generation mutation currently prevents publication.
    pub fn begin_candidate_start(
        &self,
        candidate: &GenerationCandidate,
    ) -> Result<GenerationStartup, GenerationError> {
        let _operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            return Err(GenerationError::MutationInProgress);
        }
        let pending = state
            .candidate
            .as_ref()
            .filter(|pending| {
                pending.id == candidate.id
                    && Arc::ptr_eq(&pending.generation, &candidate.generation)
            })
            .ok_or(GenerationError::CandidateSuperseded)?;
        if state.starting_candidate.is_some()
            || state
                .active
                .as_ref()
                .is_some_and(|active| active.mutations.load(Ordering::Acquire) != 0)
        {
            return Err(GenerationError::MutationInProgress);
        }
        let reservation_token = self.next_reservation_token();
        state.starting_candidate = Some(CandidateStartReservation {
            candidate_id: pending.id,
            phase: CandidateStartPhase::Starting,
            token: reservation_token,
        });
        Ok(GenerationStartup {
            candidate: candidate.clone(),
            manager: self.clone(),
            reservation_token,
            completed: false,
        })
    }

    fn cancel_startup(&self, candidate: &GenerationCandidate, reservation_token: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.cancel_startup_locked(&mut state, candidate, reservation_token);
    }

    fn cancel_startup_locked(
        &self,
        state: &mut GenerationState,
        candidate: &GenerationCandidate,
        reservation_token: u64,
    ) {
        let Some(starting) = state.starting_candidate else {
            return;
        };
        if starting.candidate_id != candidate.id || starting.token != reservation_token {
            return;
        }
        match starting.phase {
            CandidateStartPhase::Starting => {
                state.starting_candidate = None;
            }
            CandidateStartPhase::RuntimeOwned => {
                candidate.generation.cancel_runtime_start();
                state.starting_candidate = None;
                self.quarantine_locked(state, candidate, "runtime_start");
            }
            CandidateStartPhase::Activating => {}
        }
    }

    /// Restores the retained previous generation as active.
    ///
    /// # Errors
    ///
    /// Returns an error when no previous generation is retained.
    pub fn rollback(&self) -> Result<GenerationCandidate, GenerationError> {
        self.rollback_with_deadline_internal(None)
    }

    pub(crate) fn rollback_with_deadline(
        &self,
        deadline: Instant,
    ) -> Result<GenerationCandidate, GenerationError> {
        self.rollback_with_deadline_internal(Some(deadline))
    }

    fn rollback_with_deadline_internal(
        &self,
        deadline: Option<Instant>,
    ) -> Result<GenerationCandidate, GenerationError> {
        let _operation = self.operations.acquire();
        let previous_config = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.shutting_down {
                return Err(GenerationError::MutationInProgress);
            }
            let previous = state.previous.clone().ok_or(GenerationError::NoPrevious)?;
            if state.quarantined_revision.as_ref() == Some(&previous.revision.candidate) {
                return Err(GenerationError::QuarantinedRevision);
            }
            previous
        };
        let active_reservations = previous_config.reservations().clone();
        let document = ResolvedConfigDocument {
            authored_revision: previous_config.revision.disk.clone(),
            effective_revision: previous_config.revision.candidate.clone(),
            validated_config: (*previous_config.config).clone(),
            format: ConfigFormat::Kdl,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: String::new(),
            diagnostics: Vec::new(),
        };
        let prepared = PreparedGeneration::prepare_from_with_deadline(
            document,
            ListenerSource::BindOrReuse {
                previous: Some(&active_reservations),
                process: self.process.clone(),
            },
            deadline,
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prepared = match prepared {
            Ok(PreparationOutput::Prepared(prepared))
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) =>
            {
                let error = GenerationError::PreparationTimedOut;
                drop(prepared);
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                return Err(error);
            }
            Ok(PreparationOutput::Prepared(prepared)) => prepared,
            Ok(PreparationOutput::Validated) => {
                unreachable!("rollback preparation returned validation")
            }
            Err(error) => {
                if !matches!(error, GenerationError::PreparationTimedOut) {
                    state.quarantined_revision = Some(previous_config.revision.candidate.clone());
                }
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                state.last_failure = Some(error.code());
                return Err(error);
            }
        };
        let candidate = GenerationCandidate {
            generation: Arc::new(RuntimeGeneration::activate(prepared)),
            id: self.next_candidate_id.fetch_add(1, Ordering::Relaxed) + 1,
        };
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let error = GenerationError::PreparationTimedOut;
            drop(candidate);
            self.counters.failures.fetch_add(1, Ordering::Relaxed);
            state.last_failure = Some(error.code());
            return Err(error);
        }
        state.candidate = Some(candidate.clone());
        state.quarantined_revision = None;
        state.last_failure = None;
        self.counters.rollbacks.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_rollback",
            "prepared",
            Some(&candidate.revision().candidate),
        );
        Ok(candidate)
    }

    pub fn quarantine(&self, candidate: &GenerationCandidate, failure: &'static str) {
        let _operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.candidate.as_ref().is_some_and(|pending| {
            pending.id == candidate.id && Arc::ptr_eq(&pending.generation, &candidate.generation)
        }) {
            if let Some(starting) = state
                .starting_candidate
                .filter(|starting| starting.candidate_id == candidate.id)
            {
                if starting.phase != CandidateStartPhase::Starting {
                    candidate.generation.cancel_runtime_start();
                }
                if starting.phase != CandidateStartPhase::Activating {
                    state.starting_candidate = None;
                }
            }
            self.quarantine_locked(&mut state, candidate, failure);
        }
    }

    fn quarantine_locked(
        &self,
        state: &mut GenerationState,
        candidate: &GenerationCandidate,
        failure: &'static str,
    ) {
        if !candidate_matches(state.candidate.as_ref(), candidate) {
            return;
        }
        state.candidate = None;
        state.quarantined_revision = Some(candidate.revision().candidate.clone());
        state.last_failure = Some(failure);
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        crate::operational_event::emit(
            "generation_start",
            "quarantined",
            Some(&candidate.revision().candidate),
        );
    }

    #[must_use]
    pub fn candidate(&self) -> Option<GenerationCandidate> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate
            .clone()
    }

    pub(crate) fn observe_disk_revision(&self, revision: AuthoredRevision) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .disk_revision = Some(revision);
    }

    /// Acquires an active-generation permit that prevents publication until it is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when no generation is active or the expected revision is stale.
    pub fn begin_mutation(
        &self,
        expected_revision: &EffectiveRevision,
    ) -> Result<GenerationMutation, GenerationError> {
        self.begin_active_mutation(Some(expected_revision))
    }

    /// Acquires the shared active-generation permit required before mutating canonical config.
    ///
    /// # Errors
    ///
    /// Returns an error when shutdown or generation publication prevents a new mutation.
    pub fn begin_config_mutation(&self) -> Result<GenerationMutation, GenerationError> {
        self.begin_active_mutation(None)
    }

    fn begin_active_mutation(
        &self,
        expected_revision: Option<&EffectiveRevision>,
    ) -> Result<GenerationMutation, GenerationError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutting_down {
            return Err(GenerationError::MutationInProgress);
        }
        let active = state.active.as_ref().ok_or(GenerationError::NoActive)?;
        if state.starting_candidate.is_some() {
            return Err(GenerationError::MutationInProgress);
        }
        if expected_revision.is_some_and(|expected| active.revision.candidate != *expected) {
            return Err(GenerationError::RevisionConflict);
        }
        active.mutations.fetch_add(1, Ordering::AcqRel);
        Ok(GenerationMutation {
            generation: Arc::clone(active),
        })
    }

    #[must_use]
    pub fn active(&self) -> Option<Arc<RuntimeGeneration>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .clone()
    }

    #[must_use]
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let generations = self.reserve_shutdown();
        let mut recorder_shutdowns = Self::initiate_rtmp_shutdown(&generations, deadline);
        self.collect_recorder_cleanups(deadline, &mut recorder_shutdowns);
        let mut drained = true;
        for generation in &generations {
            let remaining = deadline.saturating_duration_since(Instant::now());
            drained &= generation.drain(remaining);
        }
        for shutdown in &recorder_shutdowns {
            drained &= shutdown.wait_until(deadline);
        }
        self.prune_completed();
        drained
    }

    /// Establishes terminal admission and returns recorder completion handles without waiting.
    #[must_use]
    pub fn begin_shutdown(&self, deadline: Instant) -> Vec<RtmpShutdown> {
        let generations = self.reserve_shutdown();
        let mut shutdowns = Self::initiate_rtmp_shutdown(&generations, deadline);
        self.collect_recorder_cleanups(deadline, &mut shutdowns);
        shutdowns
    }

    fn collect_recorder_cleanups(&self, deadline: Instant, shutdowns: &mut Vec<RtmpShutdown>) {
        let retirement_work = {
            let mut retirements = self
                .rtmp_retirements
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retirements.prune_completed();
            retirements.take_shutdown_work()
        };
        for work in retirement_work {
            let shutdown = work.initiate(deadline);
            push_unique_shutdown(shutdowns, &shutdown);
        }
    }

    pub fn prune_completed(&self) {
        self.rtmp_retirements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prune_completed();
    }

    fn reserve_shutdown(&self) -> Vec<Arc<RuntimeGeneration>> {
        let _operation = self.operations.acquire();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shutting_down = true;
        let mut generations = Vec::with_capacity(3);
        if let Some(active) = &state.active {
            push_unique_generation(&mut generations, active);
        }
        if let Some(previous) = &state.previous {
            push_unique_generation(&mut generations, previous);
        }
        let starting = state.starting_candidate;
        if let Some(candidate) = state.candidate.take() {
            if starting
                .is_some_and(|reservation| reservation.phase != CandidateStartPhase::Starting)
            {
                candidate.generation.cancel_runtime_start();
            }
            push_unique_generation(&mut generations, &candidate.generation);
        }
        if starting.is_none_or(|reservation| reservation.phase != CandidateStartPhase::Activating) {
            state.starting_candidate = None;
        }
        for generation in generations {
            push_unique_generation(&mut state.shutdown_generations, &generation);
        }
        state.shutdown_generations.clone()
    }

    fn initiate_rtmp_shutdown(
        generations: &[Arc<RuntimeGeneration>],
        deadline: Instant,
    ) -> Vec<RtmpShutdown> {
        generations
            .iter()
            .map(|generation| generation.initiate_rtmp_shutdown(deadline))
            .collect()
    }

    #[must_use]
    pub fn status(&self) -> GenerationStatus {
        self.prune_completed();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        GenerationStatus {
            build_version: crate::cli::BUILD_VERSION,
            disk_revision: state.disk_revision.clone(),
            candidate_revision: state
                .candidate
                .as_ref()
                .map(|candidate| candidate.revision().candidate.clone()),
            active_revision: state
                .active
                .as_ref()
                .map(|active| active.revision.candidate.clone()),
            previous_revision: state
                .previous
                .as_ref()
                .map(|previous| previous.revision.candidate.clone()),
            quarantined_revision: state.quarantined_revision.clone(),
            active_accepting: state
                .active
                .as_ref()
                .is_some_and(|active| active.accepting()),
            degraded: state.last_failure.is_some(),
            last_failure: state.last_failure,
            prepares: self.counters.prepares.load(Ordering::Relaxed),
            activations: self.counters.activations.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
            rollbacks: self.counters.rollbacks.load(Ordering::Relaxed),
        }
    }
}

fn candidate_matches(
    pending: Option<&GenerationCandidate>,
    candidate: &GenerationCandidate,
) -> bool {
    pending.is_some_and(|pending| {
        pending.id == candidate.id && Arc::ptr_eq(&pending.generation, &candidate.generation)
    })
}

fn clear_starting_reservation(
    state: &mut GenerationState,
    candidate_id: u64,
    token: u64,
    phase: CandidateStartPhase,
) {
    if state.starting_candidate
        == Some(CandidateStartReservation {
            candidate_id,
            phase,
            token,
        })
    {
        state.starting_candidate = None;
    }
}

fn active_matches_reservation(
    state: &GenerationState,
    reservation: &ActivationReservation,
) -> bool {
    match (&state.active, &reservation.previous) {
        (Some(active), Some(previous)) => {
            Arc::ptr_eq(active, previous)
                && Some(&active.revision.candidate) == reservation.previous_revision.as_ref()
        }
        (None, None) => reservation.previous_revision.is_none(),
        (Some(_), None) | (None, Some(_)) => false,
    }
}

fn push_unique_generation(
    generations: &mut Vec<Arc<RuntimeGeneration>>,
    generation: &Arc<RuntimeGeneration>,
) {
    if !generations
        .iter()
        .any(|existing| Arc::ptr_eq(existing, generation))
    {
        generations.push(Arc::clone(generation));
    }
}

fn push_unique_shutdown(shutdowns: &mut Vec<RtmpShutdown>, shutdown: &RtmpShutdown) {
    if !shutdowns
        .iter()
        .any(|existing| existing.is_same_lifecycle(shutdown))
    {
        shutdowns.push(shutdown.clone());
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationStatus {
    pub build_version: &'static str,
    pub disk_revision: Option<AuthoredRevision>,
    pub candidate_revision: Option<EffectiveRevision>,
    pub active_revision: Option<EffectiveRevision>,
    pub previous_revision: Option<EffectiveRevision>,
    pub quarantined_revision: Option<EffectiveRevision>,
    pub active_accepting: bool,
    pub degraded: bool,
    pub last_failure: Option<&'static str>,
    pub prepares: u64,
    pub activations: u64,
    pub failures: u64,
    pub rollbacks: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("candidate runtime preparation failed: {0}")]
    Plan(#[source] Box<crate::ServicePlanError>),
    #[error("candidate runtime preparation failed")]
    RuntimePrepare,
    #[error("candidate preparation exceeded its deadline")]
    PreparationTimedOut,
    #[error("candidate listener reservation failed")]
    Listener(#[from] io::Error),
    #[error("candidate metrics preparation failed")]
    Metrics(#[from] crate::MetricsError),
    #[error("candidate RTMP preparation failed")]
    Rtmp(#[from] crate::ServicePlanError),
    #[error("candidate management token preparation failed")]
    ManagementToken,
    #[error("no prepared candidate is available")]
    NoCandidate,
    #[error("the prepared candidate was superseded")]
    CandidateSuperseded,
    #[error("an active-generation mutation is in progress")]
    MutationInProgress,
    #[error("active generation acceptors did not quiesce")]
    AcceptorQuiesce,
    #[error("no active generation is available")]
    NoActive,
    #[error("the active generation revision changed")]
    RevisionConflict,
    #[error("no previous generation is available")]
    NoPrevious,
    #[error("the previous generation revision is quarantined")]
    QuarantinedRevision,
}

impl From<RuntimeAcquisitionError> for GenerationError {
    fn from(error: RuntimeAcquisitionError) -> Self {
        match error {
            RuntimeAcquisitionError::ServicePlan(error) => Self::Plan(Box::new(error)),
            RuntimeAcquisitionError::PreparationTimedOut => Self::PreparationTimedOut,
        }
    }
}

fn map_rtmp_runtime_set_error(error: RtmpRuntimeSetError) -> GenerationError {
    let timed_out = matches!(
        &error,
        RtmpRuntimeSetError::PreparationTimedOut { .. } | RtmpRuntimeSetError::StartTimedOut { .. }
    );
    drop(error);
    if timed_out {
        GenerationError::PreparationTimedOut
    } else {
        GenerationError::RuntimePrepare
    }
}

impl GenerationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Plan(_) => "service_plan_prepare",
            Self::RuntimePrepare => "runtime_prepare",
            Self::PreparationTimedOut => "generation_prepare_timeout",
            Self::Listener(_) => "listener_reservation",
            Self::Metrics(_) => "metrics_prepare",
            Self::Rtmp(_) => "rtmp_prepare",
            Self::ManagementToken => "management_token_prepare",
            Self::NoCandidate => "candidate_missing",
            Self::CandidateSuperseded => "candidate_superseded",
            Self::MutationInProgress => "mutation_in_progress",
            Self::AcceptorQuiesce => "acceptor_quiesce",
            Self::NoActive => "generation_unavailable",
            Self::RevisionConflict => "generation_conflict",
            Self::NoPrevious => "previous_missing",
            Self::QuarantinedRevision => "generation_quarantined",
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        fs,
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
        thread,
    };

    use oxiroute_config::{ConfigDraft, Listener, ListenerBind, Management, Protocol, Stats};
    use tempfile::TempDir;

    use crate::config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome};

    use super::*;

    fn document() -> ResolvedConfigDocument {
        document_with_max_connections(None)
    }

    fn disk_cache_document(root: &std::path::Path) -> ResolvedConfigDocument {
        let source = format!(
            r#"
return {{
  version = 1,
  listeners = {{}},
  cache_stores = {{
    {{
      name = "disk",
      type = "disk",
      root_directory = "{}",
      max_bytes = 1048576,
      max_files = 128,
      max_object_bytes = 65536,
    }},
  }},
  upstream_pools = {{
    {{
      name = "origin",
      endpoints = {{ {{ type = "socket", address = "127.0.0.1:3000" }} }},
    }},
  }},
  http_services = {{
    {{
      name = "web",
      routes = {{
        {{
          path = {{ kind = "segment_prefix", value = "/" }},
          action = {{
            type = "proxy",
            upstream_pool = "origin",
            policy = {{ cache = {{ store = "disk" }} }},
          }},
        }},
      }},
    }},
  }},
}}
"#,
            root.display()
        );
        let config = oxiroute_config_source::load_lua(&source)
            .expect("disk cache configuration")
            .to_draft();
        document_for(&config)
    }

    #[test]
    fn provisional_generation_transaction_rolls_back_in_reverse_order() {
        let config = Arc::new(document().validated_config);
        let inventory = ListenerInventory::compile(&config);
        let plan = runtime_plan(&config).expect("runtime plan");
        let acquired = acquire_runtime_services(&plan).expect("runtime acquisition");
        let reservations = ListenerReservations::prepare(&config, None).expect("reservations");
        let metrics = RuntimeMetrics::new();
        let registration = metrics
            .register_inventory(inventory.entries())
            .expect("listener registration");
        let rtmp = PreparedGeneration::prepare_rtmp(acquired.services()).expect("RTMP preparation");
        PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());
        let transaction = PreparedGenerationTransaction {
            drop_probes: vec![
                PreparedTransactionDropProbe::new("plan"),
                PreparedTransactionDropProbe::new("acquisition"),
                PreparedTransactionDropProbe::new("reservations"),
                PreparedTransactionDropProbe::new("listener_registration"),
                PreparedTransactionDropProbe::new("rtmp_prepared"),
            ],
            reservations_precede_plan: false,
            #[cfg(target_os = "linux")]
            descriptors: None,
            rtmp: Some(rtmp),
            listener_registration: Some(registration),
            reservations: Some(reservations),
            acquired: Some(acquired),
            plan: Some(plan),
        };

        drop(transaction);

        assert_eq!(
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
            [
                "rtmp_prepared",
                "listener_registration",
                "reservations",
                "acquisition",
                "plan",
            ]
        );
    }

    #[test]
    fn every_prepared_transaction_boundary_rolls_back_completed_resources() {
        for (fault, expected) in [
            (
                PreparedTransactionFault::Reservations,
                vec!["reservations", "acquisition", "plan"],
            ),
            (
                PreparedTransactionFault::ListenerRegistration,
                vec![
                    "listener_registration",
                    "reservations",
                    "acquisition",
                    "plan",
                ],
            ),
            (
                PreparedTransactionFault::RtmpPreparation,
                vec![
                    "rtmp_prepared",
                    "listener_registration",
                    "reservations",
                    "acquisition",
                    "plan",
                ],
            ),
        ] {
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());
            let process = ProcessRuntime::new(None);
            let result = with_prepared_transaction_fault(fault, || {
                PreparedGeneration::prepare_from(
                    document(),
                    ListenerSource::BindOrReuse {
                        previous: None,
                        process: process.clone(),
                    },
                )
            });

            assert!(result.is_err(), "{fault:?}");
            assert_eq!(process.listener_count(), 0, "{fault:?}");
            assert_eq!(
                PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
                expected,
                "{fault:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn listener_sources_share_preparation_tail_without_validation_runtime_ownership() {
        use oxiroute_supervision_unix::{DescriptorManifest, DescriptorSet};

        let process = ProcessRuntime::new(None);
        let normal = PreparedGeneration::prepare_from(
            document(),
            ListenerSource::BindOrReuse {
                previous: None,
                process: process.clone(),
            },
        )
        .expect("normal preparation");
        assert!(matches!(normal, PreparationOutput::Prepared(_)));
        drop(normal);
        assert_eq!(process.listener_count(), 0);

        let validation = PreparedGeneration::prepare_from(
            document(),
            ListenerSource::Validate { previous: None },
        )
        .expect("validation preparation");
        assert!(matches!(validation, PreparationOutput::Validated));

        let manifest = DescriptorManifest::new(Vec::new()).expect("empty descriptor manifest");
        let descriptors = DescriptorSet::new(&manifest, Vec::new()).expect("empty descriptor set");
        let adopted = PreparedGeneration::prepare_from(
            document(),
            ListenerSource::Adopt {
                descriptors,
                process: process.clone(),
            },
        )
        .expect("adopted preparation");
        assert!(matches!(adopted, PreparationOutput::Prepared(_)));
        drop(adopted);
        assert_eq!(process.listener_count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adopted_common_tail_faults_release_metrics_plan_and_descriptors_in_reverse_order() {
        for (fault, expected) in [
            (
                PreparedTransactionFault::ListenerRegistration,
                vec![
                    "listener_registration",
                    "acquisition",
                    "plan",
                    "reservations",
                ],
            ),
            (
                PreparedTransactionFault::RtmpPreparation,
                vec![
                    "rtmp_prepared",
                    "listener_registration",
                    "acquisition",
                    "plan",
                    "reservations",
                ],
            ),
        ] {
            let root = TempDir::new().expect("adopted fault root");
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("adopted fault listener");
            let address = listener.local_addr().expect("adopted fault address");
            let (document, descriptors) =
                adopted_rtmp_fixture(root.path(), address, listener, false);
            let process = ProcessRuntime::new(None);
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());

            let result = with_prepared_transaction_fault(fault, || {
                PreparedGeneration::prepare_from(
                    document,
                    ListenerSource::Adopt {
                        descriptors,
                        process: process.clone(),
                    },
                )
            });

            assert!(result.is_err(), "{fault:?}");
            assert_eq!(process.listener_count(), 0, "{fault:?}");
            assert_eq!(
                PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
                expected,
                "{fault:?}"
            );
            std::net::TcpListener::bind(address).expect("adopted fault released its descriptor");
        }
    }

    #[test]
    fn adapter_generation_handle_retains_root_and_tears_it_down_exactly_once() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let PreparationOutput::Prepared(mut prepared) = PreparedGeneration::prepare_from(
            document(),
            ListenerSource::BindOrReuse {
                previous: None,
                process: ProcessRuntime::new(None),
            },
        )
        .expect("prepared generation") else {
            panic!("runtime preparation returned validation");
        };
        prepared.resources.set_drop_probe(Arc::clone(&drops));
        let generation = Arc::new(RuntimeGeneration::activate(prepared));
        let adapter = Arc::clone(&generation);

        drop(generation);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        drop(adapter);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn plan_error_preserves_code_and_redacted_rtmp_source_chain() {
        use std::error::Error as _;

        let source = oxiroute_rtmp::RtmpServicePlan::new(
            "streaming",
            4_096,
            oxiroute_rtmp::RtmpSessionLimits::default().with_max_inbound_message_size(0),
            oxiroute_rtmp::RtmpCallbackPlan::default(),
            [],
            None,
        )
        .unwrap_err();
        let error = GenerationError::Plan(Box::new(crate::ServicePlanError::RtmpPreparation(
            Box::new(source),
        )));

        assert_eq!(error.code(), "service_plan_prepare");
        assert!(error.source().is_some());
        assert!(error.source().unwrap().source().is_some());
        let rendered = error.to_string();
        assert!(rendered.contains("invalid RTMP Bound at service.inbound_limits"));
        assert!(rendered.contains("service `streaming`"));
        for secret in [
            "/secret/tenant/key.pem",
            "token=super-secret",
            "https://user:password@example.test/private",
        ] {
            assert!(!rendered.contains(secret));
        }
    }

    fn document_with_max_connections(max_connections: Option<u64>) -> ResolvedConfigDocument {
        document_for(&ConfigDraft {
            version: 1,
            max_connections,
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
        })
    }

    fn document_for(config: &ConfigDraft) -> ResolvedConfigDocument {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("oxiroute.lua");
        let config = config.clone().validate().expect("valid config");
        fs::write(
            &path,
            oxiroute_config_source::render_config(
                oxiroute_config_source::ConfigFormat::Lua,
                &config,
            )
            .expect("render"),
        )
        .expect("write");
        let ConfigLoadOutcome::Loaded(document) = CanonicalConfigCoordinator::new(path)
            .expect("coordinator")
            .load()
        else {
            panic!("load")
        };
        *document
    }

    fn colliding_listener_document() -> ResolvedConfigDocument {
        let traffic_management = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("traffic management probe")
            .local_addr()
            .expect("traffic management address");
        let traffic_stats = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("traffic stats probe")
            .local_addr()
            .expect("traffic stats address");
        let management = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("management probe")
            .local_addr()
            .expect("management address");
        let stats = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("stats probe")
            .local_addr()
            .expect("stats address");
        let mut config: ConfigDraft = serde_json::from_value(serde_json::json!({
            "version": 1,
            "listeners": [],
            "upstream_pools": [{
                "name": "origin",
                "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]
            }],
            "l4_services": [{"name": "relay", "upstream_pool": "origin"}]
        }))
        .expect("collision config");
        config.listeners = vec![
            Listener {
                name: "@management".into(),
                bind: ListenerBind::Socket {
                    address: traffic_management,
                },
                protocol: Protocol::Tcp,
                service: Some("relay".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            },
            Listener {
                name: "@stats-0".into(),
                bind: ListenerBind::Socket {
                    address: traffic_stats,
                },
                protocol: Protocol::Tcp,
                service: Some("relay".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            },
        ];
        config.management = Some(Management {
            bind: management,
            ui_dir: None,
        });
        config.stats = Some(Stats {
            binds: vec![stats],
            admin_token_file: None,
            pages: Vec::new(),
        });
        document_for(&config)
    }

    fn rtmp_failure_document(root: &std::path::Path, auto_push: bool) -> ResolvedConfigDocument {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("RTMP probe")
            .local_addr()
            .expect("RTMP address");
        let mut config: ConfigDraft = serde_json::from_value(serde_json::json!({
            "version": 1,
            "listeners": [{
                "name": "ingest",
                "bind": {"type": "socket", "address": listener},
                "protocol": "rtmp",
                "service": "live"
            }],
            "rtmp_services": [{
                "name": "live",
                "outbound_chunk_size": 4096,
                "max_inbound_message_size": 8_388_608,
                "ack_window_size": 5_000_000,
                "applications": [{
                    "name": "live",
                    "live": true,
                    "recorders": [{"name": "archive", "root_directory": root}]
                }]
            }]
        }))
        .expect("RTMP failure config");
        if auto_push {
            config.rtmp_services[0].auto_push.enabled = true;
            config.rtmp_services[0].auto_push.socket_dir = root.join("auto-push");
        }
        document_for(&config)
    }

    #[cfg(target_os = "linux")]
    fn adopted_rtmp_fixture(
        root: &std::path::Path,
        address: std::net::SocketAddr,
        listener: std::net::TcpListener,
        auto_push: bool,
    ) -> (ResolvedConfigDocument, DescriptorSet) {
        use std::os::fd::OwnedFd;

        use oxiroute_supervision_unix::{
            BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSlot,
            SlotId,
        };

        let document = rtmp_failure_document_for(root, address, auto_push);
        let manifest = DescriptorManifest::new(vec![DescriptorSlot {
            id: SlotId(0),
            role: DescriptorRole::Traffic("ingest".into()),
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(address)),
            mode: None,
        }])
        .expect("RTMP descriptor manifest");
        let descriptors = DescriptorSet::new(&manifest, vec![OwnedFd::from(listener)])
            .expect("RTMP descriptor set");
        (document, descriptors)
    }

    #[cfg(target_os = "linux")]
    fn adopted_planning_failure_fixture(
        address: std::net::SocketAddr,
        listener: std::net::TcpListener,
    ) -> (ResolvedConfigDocument, DescriptorSet) {
        use std::os::fd::OwnedFd;

        use oxiroute_supervision_unix::{
            BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSlot,
            SlotId,
        };

        let config: ConfigDraft = serde_json::from_value(serde_json::json!({
            "version": 1,
            "listeners": [{
                "name": "web",
                "bind": {"type": "socket", "address": address},
                "protocol": "http",
                "service": "web"
            }],
            "http_services": [{
                "name": "web",
                "routes": [{
                    "path": {"kind": "segment_prefix", "value": "/"},
                    "policy": {
                        "request_buffering": true,
                        "max_request_body_bytes": null
                    },
                    "action": {"type": "fixed_response", "status": 200}
                }]
            }]
        }))
        .expect("planning-failure config");
        let manifest = DescriptorManifest::new(vec![DescriptorSlot {
            id: SlotId(0),
            role: DescriptorRole::Traffic("web".into()),
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(address)),
            mode: None,
        }])
        .expect("planning-failure descriptor manifest");
        let descriptors = DescriptorSet::new(&manifest, vec![OwnedFd::from(listener)])
            .expect("planning-failure descriptor set");
        (document_for(&config), descriptors)
    }

    #[cfg(target_os = "linux")]
    fn rtmp_failure_document_for(
        root: &std::path::Path,
        address: std::net::SocketAddr,
        auto_push: bool,
    ) -> ResolvedConfigDocument {
        let mut config: ConfigDraft = serde_json::from_value(serde_json::json!({
            "version": 1,
            "listeners": [{
                "name": "ingest",
                "bind": {"type": "socket", "address": address},
                "protocol": "rtmp",
                "service": "live"
            }],
            "rtmp_services": [{
                "name": "live",
                "outbound_chunk_size": 4096,
                "max_inbound_message_size": 8_388_608,
                "ack_window_size": 5_000_000,
                "applications": [{
                    "name": "live",
                    "live": true,
                    "recorders": [{"name": "archive", "root_directory": root}]
                }]
            }]
        }))
        .expect("adopted RTMP config");
        if auto_push {
            config.rtmp_services[0].auto_push.enabled = true;
            config.rtmp_services[0].auto_push.socket_dir = root.join("auto-push");
        }
        document_for(&config)
    }

    #[test]
    fn validation_acquires_and_releases_rtmp_resources_without_starting_runtime_effects() {
        use crate::service_plan::{reset_rtmp_stage_counts, rtmp_stage_counts};

        let parent = TempDir::new().expect("stage parent");
        let media = parent.path().join("media-do-not-create");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("RTMP listener probe");
        let address = listener.local_addr().expect("RTMP listener address");
        let document = document_for(
            &serde_json::from_value(serde_json::json!({
                "version": 1,
                "listeners": [{
                    "name": "ingest",
                    "bind": {"type": "socket", "address": address},
                    "protocol": "rtmp",
                    "service": "live",
                }],
                "rtmp_services": [{
                    "name": "live",
                    "applications": [{
                        "name": "live",
                        "live": true,
                        "hls": {"root_directory": media},
                    }],
                }],
            }))
            .unwrap(),
        );
        let manager = GenerationManager::new();
        reset_rtmp_stage_counts();
        drop(listener);

        manager
            .validate_candidate(document_for(document.validated_config.as_draft()))
            .expect("RTMP preflight and listener probes");
        assert_eq!(rtmp_stage_counts(), (1, 0));
        assert!(!media.exists(), "validation must not create the media root");

        let candidate = manager.prepare(document).expect("activation preparation");
        assert_eq!(rtmp_stage_counts(), (2, 0));
        assert!(media.is_dir(), "activation preparation opens media roots");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("runtime start reservation");
        let generation = startup.claim_runtime_start().expect("RTMP start");
        assert_eq!(rtmp_stage_counts(), (2, 1));
        assert!(generation.rtmp_service("live").is_some());
        assert!(!generation.rtmp_auto_push_status().started);
        assert!(matches!(
            startup.claim_runtime_start(),
            Err(GenerationError::CandidateSuperseded | GenerationError::RuntimePrepare)
        ));
        assert_eq!(rtmp_stage_counts(), (2, 1));
    }

    #[test]
    fn candidate_failure_shutdown_releases_storage_and_allows_retry() {
        let root = TempDir::new().expect("recording root");
        let manager = GenerationManager::new();
        let candidate = manager
            .prepare(rtmp_failure_document(root.path(), false))
            .expect("candidate preparation");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("runtime start reservation");
        let generation = startup.claim_runtime_start().expect("runtime start");
        let later = Instant::now() + Duration::from_secs(1);
        let first = generation.initiate_rtmp_shutdown(later);
        let second = generation.initiate_rtmp_shutdown(later + Duration::from_secs(1));

        assert!(first.is_same_lifecycle(&second));
        assert!(first.wait_until(later));
        drop(startup);
        manager.quarantine(&candidate, "runtime_start");
        drop(generation);
        drop(candidate);

        let retry = GenerationManager::new()
            .prepare(rtmp_failure_document(root.path(), false))
            .expect("recording ownership retry");
        drop(retry);
    }

    #[test]
    fn published_generation_uses_normal_shutdown() {
        let root = TempDir::new().expect("recording root");
        let manager = GenerationManager::new();
        let candidate = manager
            .prepare(rtmp_failure_document(root.path(), false))
            .expect("candidate preparation");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("runtime start reservation");
        let generation = startup.claim_runtime_start().expect("runtime start");
        assert!(generation.mark_runtime_started());
        startup.activate().expect("candidate publication");

        assert!(manager.shutdown(Duration::from_secs(1)));
    }

    #[test]
    fn retirement_registry_deduplicates_and_prunes_completed_lifecycles() {
        let root = TempDir::new().expect("recording root");
        let manager = GenerationManager::new();
        let candidate = manager
            .prepare(rtmp_failure_document(root.path(), false))
            .expect("candidate preparation");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("runtime start reservation");
        let generation = startup.claim_runtime_start().expect("runtime start");
        let retirement = generation.rtmp_retirement();
        let mut registry = RtmpRetirementRegistry::default();

        registry.retire(retirement.clone());
        registry.retire(retirement);
        assert_eq!(registry.len(), 1);

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut work = registry.take_shutdown_work();
        assert_eq!(work.len(), 1);
        let shutdown = work.pop().expect("retirement work").initiate(deadline);
        let mut repeated_work = registry.take_shutdown_work();
        assert_eq!(repeated_work.len(), 1);
        let repeated_shutdown = repeated_work
            .pop()
            .expect("repeated retirement work")
            .initiate(deadline);
        assert!(shutdown.is_same_lifecycle(&repeated_shutdown));
        assert!(shutdown.wait_until(deadline));
        registry.prune_completed();
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn manager_shutdown_retires_evicted_lifecycles_without_process_handles() {
        let root = TempDir::new().expect("recording root");
        let manager = GenerationManager::new();
        let first = manager
            .prepare(rtmp_failure_document(root.path(), false))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(rtmp_failure_document(root.path(), false))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");

        let deadline = Instant::now() + Duration::from_secs(1);
        drop(manager.begin_shutdown(deadline));
        assert!(manager.shutdown(Duration::from_secs(1)));
    }

    #[test]
    fn rtmp_services_start_in_plan_order_and_partial_failure_rolls_back_in_reverse() {
        use crate::service_plan::{
            reset_rtmp_stage_counts, rtmp_start_events, with_rtmp_start_failure,
        };

        let first = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("first RTMP probe")
            .local_addr()
            .expect("first RTMP address");
        let second = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("second RTMP probe")
            .local_addr()
            .expect("second RTMP address");
        let config: ConfigDraft = serde_json::from_value(serde_json::json!({
            "version": 1,
            "listeners": [
                {
                    "name": "first-ingest",
                    "bind": {"type": "socket", "address": first},
                    "protocol": "rtmp",
                    "service": "first"
                },
                {
                    "name": "second-ingest",
                    "bind": {"type": "socket", "address": second},
                    "protocol": "rtmp",
                    "service": "second"
                }
            ],
            "rtmp_services": [
                {"name": "first", "applications": [{"name": "live", "live": true}]},
                {"name": "second", "applications": [{"name": "live", "live": true}]}
            ]
        }))
        .expect("ordered RTMP config");
        let manager = GenerationManager::new();
        reset_rtmp_stage_counts();
        let candidate = manager
            .prepare(document_for(&config))
            .expect("RTMP preparation");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("runtime start reservation");

        let result = with_rtmp_start_failure("second", || startup.claim_runtime_start());

        assert!(result.is_err());
        assert_eq!(
            rtmp_start_events(),
            ["start:first", "start:second", "rollback:first"]
        );
        assert_eq!(manager.process.listener_count(), 0);
    }

    #[test]
    fn validation_reports_rtmp_store_and_runtime_construction_failures_without_starting() {
        use crate::service_plan::{
            RtmpRuntimeFault, reset_rtmp_stage_counts, rtmp_stage_counts, with_rtmp_runtime_fault,
        };

        for fault in [RtmpRuntimeFault::RecorderStore, RtmpRuntimeFault::AutoPush] {
            let root = TempDir::new().expect("RTMP validation root");
            let document = rtmp_failure_document(root.path(), fault == RtmpRuntimeFault::AutoPush);
            let manager = GenerationManager::new();
            reset_rtmp_stage_counts();

            let result = with_rtmp_runtime_fault(fault, || manager.validate_candidate(document));

            assert!(result.is_err(), "{fault:?} validation must fail");
            assert_eq!(rtmp_stage_counts(), (1, 0), "{fault:?}");
            assert_eq!(manager.process.listener_count(), 0);
        }
    }

    #[test]
    fn later_rtmp_failures_roll_back_process_listener_entries_and_allow_retry() {
        use crate::service_plan::{RtmpRuntimeFault, with_rtmp_runtime_fault};

        for fault in [RtmpRuntimeFault::RecorderStore, RtmpRuntimeFault::AutoPush] {
            let root = TempDir::new().expect("recording root");
            let process = ProcessRuntime::new(None);
            let document = rtmp_failure_document(root.path(), fault == RtmpRuntimeFault::AutoPush);
            let failed = with_rtmp_runtime_fault(fault, || match fault {
                RtmpRuntimeFault::RecorderStore => PreparedGeneration::prepare_from(
                    document,
                    ListenerSource::BindOrReuse {
                        previous: None,
                        process: process.clone(),
                    },
                )
                .map(drop),
                RtmpRuntimeFault::AutoPush => {
                    let manager = GenerationManager::new();
                    let candidate = manager.prepare(document)?;
                    let mut startup = manager.begin_candidate_start(&candidate)?;
                    let result = startup.claim_runtime_start().map(drop);
                    assert_eq!(manager.process.listener_count(), 0);
                    result
                }
                RtmpRuntimeFault::None => unreachable!(),
            });

            assert!(failed.is_err());
            assert_eq!(process.listener_count(), 0);

            let retry = PreparedGeneration::prepare_from(
                rtmp_failure_document(root.path(), fault == RtmpRuntimeFault::AutoPush),
                ListenerSource::BindOrReuse {
                    previous: None,
                    process: process.clone(),
                },
            )
            .expect("successful retry");
            let PreparationOutput::Prepared(retry) = retry else {
                panic!("runtime preparation returned validation");
            };
            assert_eq!(process.listener_count(), 1);
            drop(retry);
            assert_eq!(process.listener_count(), 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adopted_rtmp_failures_roll_back_metrics_descriptors_and_recording_ownership() {
        use crate::service_plan::{RtmpRuntimeFault, with_rtmp_runtime_fault};

        for fault in [RtmpRuntimeFault::RecorderStore, RtmpRuntimeFault::AutoPush] {
            let manager = GenerationManager::new_supervised();
            let active_candidate = manager.prepare(document()).expect("active candidate");
            let active = manager
                .activate(&active_candidate)
                .expect("active generation");
            let active_revision = active.revision().candidate.clone();
            let root = TempDir::new().expect("recording root");
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("adopted RTMP listener");
            let address = listener.local_addr().expect("adopted RTMP address");
            let (document, descriptors) = adopted_rtmp_fixture(
                root.path(),
                address,
                listener,
                fault == RtmpRuntimeFault::AutoPush,
            );

            let failed = with_rtmp_runtime_fault(fault, || match fault {
                RtmpRuntimeFault::RecorderStore => {
                    manager.prepare_adopted(document, descriptors).map(drop)
                }
                RtmpRuntimeFault::AutoPush => {
                    let candidate = manager.prepare_adopted(document, descriptors)?;
                    assert_eq!(manager.process.listener_count(), 1);
                    assert!(std::net::TcpListener::bind(address).is_err());
                    let mut startup = manager.begin_candidate_start(&candidate)?;
                    let result = startup.claim_runtime_start().map(drop);
                    assert!(matches!(
                        startup.claim_runtime_start(),
                        Err(GenerationError::CandidateSuperseded | GenerationError::RuntimePrepare)
                    ));
                    drop(startup);
                    drop(candidate);
                    result
                }
                RtmpRuntimeFault::None => unreachable!(),
            });

            assert!(failed.is_err());
            assert_eq!(manager.process.listener_count(), 0);
            assert_eq!(
                manager.active().unwrap().revision().candidate,
                active_revision
            );

            let retry_listener = std::net::TcpListener::bind(address)
                .expect("failed adoption released its listener descriptor");
            let (retry_document, retry_descriptors) = adopted_rtmp_fixture(
                root.path(),
                address,
                retry_listener,
                fault == RtmpRuntimeFault::AutoPush,
            );
            let retry = manager
                .prepare_adopted(retry_document, retry_descriptors)
                .expect("adopted RTMP retry preparation");
            assert_eq!(manager.process.listener_count(), 1);
            let mut startup = manager
                .begin_candidate_start(&retry)
                .expect("adopted RTMP retry reservation");
            assert!(startup.claim_runtime_start().is_ok());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adopted_planning_failure_releases_the_consumed_descriptor_once() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted listener");
        let address = listener.local_addr().expect("adopted listener address");
        let (document, descriptors) = adopted_planning_failure_fixture(address, listener);
        PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().clear());

        let result = PreparedGeneration::prepare_from(
            document,
            ListenerSource::Adopt {
                descriptors,
                process: ProcessRuntime::new(None),
            },
        );
        let Err(error) = result else {
            panic!("runtime planning must fail after descriptor adoption");
        };

        assert!(matches!(error, GenerationError::Plan(_)));
        assert_eq!(
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow().clone()),
            ["reservations"]
        );
        let rebound = std::net::TcpListener::bind(address)
            .expect("planning failure released the adopted descriptor");
        drop(rebound);
    }

    #[test]
    fn generation_checks_and_listener_reservation_precede_recording_store_acquisition() {
        use crate::service_plan::{reset_rtmp_stage_counts, rtmp_stage_counts};

        let manager = GenerationManager::new();
        let active_candidate = manager.prepare(document()).expect("active candidate");
        let active = manager
            .activate(&active_candidate)
            .expect("active generation");
        let active_revision = active.revision().candidate.clone();
        let root = TempDir::new().expect("recording root");

        let mut token_failure = rtmp_failure_document(root.path(), false)
            .validated_config
            .to_draft();
        let stats_address = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("stats probe")
            .local_addr()
            .expect("stats address");
        token_failure.stats = Some(Stats {
            binds: vec![stats_address],
            admin_token_file: Some(root.path().join("missing-token")),
            pages: Vec::new(),
        });
        reset_rtmp_stage_counts();
        assert!(manager.prepare(document_for(&token_failure)).is_err());
        assert_eq!(rtmp_stage_counts(), (0, 0));
        assert_eq!(
            manager.active().unwrap().revision().candidate,
            active_revision
        );

        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
        let mut listener_failure = rtmp_failure_document(root.path(), false)
            .validated_config
            .to_draft();
        listener_failure.listeners[0].bind = ListenerBind::Socket {
            address: occupied.local_addr().expect("occupied address"),
        };
        reset_rtmp_stage_counts();
        assert!(manager.prepare(document_for(&listener_failure)).is_err());
        assert_eq!(rtmp_stage_counts(), (0, 0));
        assert_eq!(
            manager.active().unwrap().revision().candidate,
            active_revision
        );
    }

    #[test]
    fn expired_preparation_deadline_drops_resources_without_quarantining_revision() {
        let manager = GenerationManager::new();
        let prepared_document = document();

        let Err(error) = manager.prepare_with_deadline(prepared_document, Instant::now()) else {
            panic!("expired preparation deadline succeeded")
        };

        assert!(matches!(error, GenerationError::PreparationTimedOut));
        let status = manager.status();
        assert_eq!(status.quarantined_revision, None);
        assert_eq!(status.last_failure, Some("generation_prepare_timeout"));
        assert!(status.disk_revision.is_some());
        assert!(manager.candidate().is_none());
        assert!(manager.prepare(document()).is_ok());
    }

    #[test]
    fn contended_preparation_deadline_expires_without_candidate_or_quarantine_and_retries() {
        let manager = GenerationManager::new();
        let authority = manager.operations.acquire();
        let deadline = Instant::now() + Duration::from_millis(25);

        let Err(error) = manager.prepare_with_deadline(document(), deadline) else {
            panic!("contended preparation succeeded")
        };

        assert!(Instant::now() >= deadline);
        assert!(matches!(error, GenerationError::PreparationTimedOut));
        let status = manager.status();
        assert_eq!(status.candidate_revision, None);
        assert_eq!(status.quarantined_revision, None);
        assert_eq!(status.last_failure, Some("generation_prepare_timeout"));
        assert_eq!(status.failures, 1);
        drop(authority);
        assert!(manager.prepare(document()).is_ok());
    }

    #[test]
    fn disk_registry_wait_timeout_is_retryable_without_quarantine() {
        let directory = TempDir::new().expect("disk cache root");
        let root = directory.path().join("cache");
        let (hook, reached, release) =
            crate::service_plan::install_disk_registry_opening_hook(root.clone());
        let opener_manager = GenerationManager::new();
        let opener_thread_manager = opener_manager.clone();
        let opener_document = disk_cache_document(&root);
        let opener = thread::spawn(move || opener_thread_manager.prepare(opener_document));
        reached.wait();

        let manager = GenerationManager::new();
        let deadline = Instant::now() + Duration::from_millis(25);
        let Err(error) = manager.prepare_with_deadline(disk_cache_document(&root), deadline) else {
            panic!("disk registry waiter did not time out")
        };

        assert!(matches!(error, GenerationError::PreparationTimedOut));
        assert!(Instant::now() >= deadline);
        let status = manager.status();
        assert_eq!(status.candidate_revision, None);
        assert_eq!(status.quarantined_revision, None);
        assert_eq!(status.last_failure, Some("generation_prepare_timeout"));
        assert_eq!(status.failures, 1);

        release.wait();
        opener
            .join()
            .expect("disk backend opener thread")
            .expect("disk backend opener generation");
        drop(hook);
        manager
            .prepare(disk_cache_document(&root))
            .expect("disk registry timeout retry");
    }

    #[test]
    fn ordinary_preparation_failure_quarantines_even_after_deadline() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
        let address = occupied.local_addr().expect("occupied address");
        let mut config = document().validated_config.to_draft();
        let service = serde_json::from_value(serde_json::json!({
            "name": "occupied-service",
            "routes": [{
                "path": {"kind": "segment_prefix", "value": "/"},
                "action": {"type": "fixed_response", "status": 200}
            }]
        }))
        .expect("occupied service");
        config.http_services.push(service);
        config.listeners.push(oxiroute_config::Listener {
            name: "occupied".into(),
            bind: oxiroute_config::ListenerBind::Socket { address },
            protocol: oxiroute_config::Protocol::Http,
            service: Some("occupied-service".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        let document = document_for(&config);
        let revision = document.effective_revision.clone();

        let manager = GenerationManager::new();
        let Err(error) = manager.prepare_with_deadline(document, Instant::now()) else {
            panic!("occupied listener preparation succeeded")
        };
        assert!(matches!(error, GenerationError::Listener(_)));
        assert_eq!(manager.status().quarantined_revision, Some(revision));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn expired_adopted_preparation_deadline_releases_adopted_descriptors() {
        let root = TempDir::new().expect("adopted deadline root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted listener");
        let address = listener.local_addr().expect("adopted address");
        let (mut document, descriptors) =
            adopted_rtmp_fixture(root.path(), address, listener, false);
        let mut config = document.validated_config.to_draft();
        config.rtmp_services[0].applications[0].recorders.clear();
        document.validated_config = config.validate().expect("recording-free adopted config");
        let manager = GenerationManager::new_supervised();

        let Err(error) =
            manager.prepare_adopted_with_deadline(document, descriptors, Instant::now())
        else {
            panic!("expired adopted preparation succeeded")
        };

        assert!(matches!(error, GenerationError::PreparationTimedOut));
        assert_eq!(manager.process.listener_count(), 0);
        assert!(manager.status().quarantined_revision.is_none());
        std::net::TcpListener::bind(address).expect("adopted descriptor was released");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn contended_adopted_preparation_deadline_releases_descriptors_and_retries() {
        let root = TempDir::new().expect("adopted contention root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("adopted listener");
        let address = listener.local_addr().expect("adopted address");
        let (mut document, descriptors) =
            adopted_rtmp_fixture(root.path(), address, listener, false);
        let mut config = document.validated_config.to_draft();
        config.rtmp_services[0].applications[0].recorders.clear();
        document.validated_config = config.validate().expect("recording-free adopted config");
        let manager = GenerationManager::new_supervised();
        let authority = manager.operations.acquire();

        let Err(error) = manager.prepare_adopted_with_deadline(
            document,
            descriptors,
            Instant::now() + Duration::from_millis(25),
        ) else {
            panic!("contended adopted preparation succeeded")
        };

        assert!(matches!(error, GenerationError::PreparationTimedOut));
        assert!(manager.candidate().is_none());
        assert!(manager.status().quarantined_revision.is_none());
        let retry_listener =
            std::net::TcpListener::bind(address).expect("timed-out adoption released descriptor");
        let (mut retry_document, retry_descriptors) =
            adopted_rtmp_fixture(root.path(), address, retry_listener, false);
        let mut retry_config = retry_document.validated_config.to_draft();
        retry_config.rtmp_services[0].applications[0]
            .recorders
            .clear();
        retry_document.validated_config = retry_config
            .validate()
            .expect("recording-free retry config");
        drop(authority);
        assert!(
            manager
                .prepare_adopted(retry_document, retry_descriptors)
                .is_ok()
        );
    }

    #[test]
    fn expired_rollback_deadline_is_retryable_without_quarantine() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second");
        manager.activate(&second).expect("second activation");

        let Err(error) = manager.rollback_with_deadline(Instant::now()) else {
            panic!("expired rollback preparation succeeded")
        };

        assert!(matches!(error, GenerationError::PreparationTimedOut));
        assert!(manager.candidate().is_none());
        assert!(manager.status().quarantined_revision.is_none());
        assert!(manager.rollback().is_ok());
    }

    #[test]
    fn public_stats_page_name_collision_fails_before_process_registration() {
        let mut config = colliding_listener_document().validated_config.to_draft();
        config.listeners[0].name = "@stats-page-0".into();
        config
            .stats
            .as_mut()
            .expect("stats")
            .pages
            .push(oxiroute_config::StatsPage {
                bind: std::net::TcpListener::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap(),
                uri_prefix: "/stats".into(),
                refresh_ms: 1_000,
                admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            });
        let process = ProcessRuntime::new(None);

        assert!(matches!(
            PreparedGeneration::prepare_from(
                document_for(&config),
                ListenerSource::BindOrReuse {
                    previous: None,
                    process: process.clone(),
                },
            ),
            Err(GenerationError::Metrics(MetricsError::DuplicateListener(name)))
                if name == "@stats-page-0"
        ));
        assert_eq!(process.listener_count(), 0);
    }

    #[test]
    fn prepared_generation_uses_typed_listener_identity_for_colliding_display_names() {
        let document = colliding_listener_document();
        let config = Arc::new(document.validated_config);
        let inventory = ListenerInventory::compile(&config);
        let plan = runtime_plan(&config).expect("collision runtime plan");
        let acquired = acquire_runtime_services(&plan).expect("collision runtime acquisition");
        let rtmp = PreparedGeneration::prepare_rtmp(acquired.services())
            .expect("collision RTMP preparation");
        let reservations =
            ListenerReservations::prepare(&config, None).expect("collision listener reservations");
        let process = ProcessRuntime::new(None);
        let metrics = RuntimeMetrics::for_process(process.clone());
        let registration = metrics
            .register_inventory(inventory.entries())
            .expect("collision inventory registration");
        let prepared = PreparedGeneration {
            config,
            inventory,
            metrics,
            resources: GenerationResources::commit(
                plan,
                acquired,
                reservations,
                registration,
                rtmp,
            ),
            revision: GenerationRevision {
                disk: document.authored_revision,
                candidate: document.effective_revision,
            },
        };
        let generation = RuntimeGeneration::activate(prepared);

        assert_eq!(
            generation
                .metrics
                .internal_listener_snapshots()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            generation
                .traffic_listener_metrics("@management")
                .unwrap()
                .name(),
            "@management"
        );
        assert_eq!(
            generation.management_listener_metrics().unwrap().name(),
            "@management"
        );
        assert_eq!(
            generation
                .traffic_listener_metrics("@stats-0")
                .unwrap()
                .name(),
            "@stats-0"
        );
        assert_eq!(
            generation.stats_listener_metrics(0).unwrap().name(),
            "@stats-0"
        );
        assert_eq!(generation.metrics.snapshot().unwrap().listeners.len(), 2);
        assert_eq!(process.listener_count(), 4);
    }

    fn claim_and_mark_runtime_started(startup: &mut GenerationStartup) -> Arc<RuntimeGeneration> {
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        assert!(generation.mark_runtime_started());
        generation
    }

    fn pause_activation_at(
        manager: &GenerationManager,
        point: ActivationHookPoint,
    ) -> (mpsc::Receiver<()>, Arc<Barrier>) {
        let (paused, paused_rx) = mpsc::sync_channel(1);
        let release = Arc::new(Barrier::new(2));
        let hook_release = Arc::clone(&release);
        let notified = Arc::new(AtomicBool::new(false));
        manager.set_activation_hook(Arc::new(move |observed| {
            if observed == point && !notified.swap(true, Ordering::AcqRel) {
                paused.send(()).expect("activation pause receiver");
                hook_release.wait();
            }
        }));
        (paused_rx, release)
    }

    #[test]
    fn activation_rollback_and_failed_activation_preserve_published_state() {
        let manager = GenerationManager::new();
        let first_candidate = manager.prepare(document()).expect("prepare first");
        let first = manager.activate(&first_candidate).expect("activate first");
        let second_candidate = manager.prepare(document()).expect("prepare second");
        let second = manager
            .activate(&second_candidate)
            .expect("activate second");

        assert!(!first.accepting());
        assert!(second.accepting());
        let rollback = manager.rollback().expect("rollback");
        assert_eq!(rollback.revision(), first.revision());
        assert!(second.accepting());
    }

    #[test]
    fn third_activation_keeps_only_the_immediately_previous_revision_rollbackable() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");
        manager.activate(&third).expect("third activation");

        let rollback = manager.rollback().expect("rollback candidate");

        assert_eq!(rollback.revision(), second.revision());
        assert_ne!(rollback.revision(), first.revision());
    }

    #[test]
    fn bounded_drain_waits_for_all_protocol_reference_kinds() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let references = [
            RuntimeReferenceKind::ForwardHttp1,
            RuntimeReferenceKind::ForwardHttp3,
            RuntimeReferenceKind::Http1,
            RuntimeReferenceKind::Http2,
            RuntimeReferenceKind::WebSocket,
            RuntimeReferenceKind::Tcp,
            RuntimeReferenceKind::Rtmp,
            RuntimeReferenceKind::Udp,
            RuntimeReferenceKind::Http3,
        ]
        .map(|kind| generation.begin_reference(kind).expect("reference"));
        let draining = Arc::clone(&generation);
        let waiter = thread::spawn(move || draining.drain(Duration::from_secs(1)));
        thread::sleep(Duration::from_millis(20));
        assert!(!generation.accepting());
        drop(references);
        assert!(waiter.join().expect("drain thread"));
    }

    #[test]
    fn admission_claim_and_protocol_reference_have_independent_lifetimes() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let admission = generation.begin_admission().expect("admission claim");

        assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 0);

        let reference = generation
            .begin_reference(RuntimeReferenceKind::Http1)
            .expect("HTTP/1 reference");
        drop(admission);

        assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 1);
        assert!(!generation.drain(Duration::ZERO));

        drop(reference);
        assert!(generation.drain(Duration::from_millis(100)));
    }

    #[test]
    fn owned_reference_remains_after_acceptance_closes_for_handoff() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let reference = generation.begin_owned_reference(RuntimeReferenceKind::Http1);

        generation.stop_accepting();

        assert!(!generation.accepting());
        assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 1);
        drop(reference);
        assert!(generation.drain(Duration::from_millis(100)));
    }

    #[test]
    fn listener_runtime_rolls_back_generation_ownership_when_metrics_reject() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let metrics = RuntimeMetrics::with_max_connections(Some(1));
        let listener = metrics
            .register_listener("edge", "http", "127.0.0.1:8080", Some(1))
            .expect("listener");
        let held = listener.begin_connection().expect("held connection");
        let runtime = crate::listener_runtime::ListenerRuntime::new(listener);

        assert!(
            runtime
                .admit(&generation, RuntimeReferenceKind::Http1)
                .is_err()
        );
        assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 0);
        drop(held);
        assert!(generation.drain(Duration::from_millis(100)));
    }

    #[test]
    fn listener_runtime_owned_admission_does_not_recheck_a_closed_generation_gate() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("prepare");
        let generation = manager.activate(&candidate).expect("activate");
        let metrics = RuntimeMetrics::new();
        let listener = metrics
            .register_listener("edge", "http", "127.0.0.1:8080", Some(1))
            .expect("listener");
        let runtime = crate::listener_runtime::ListenerRuntime::new(listener);

        generation.stop_accepting();
        let lease = runtime
            .admit_owned(&generation, RuntimeReferenceKind::Http1)
            .expect("owned admission after gate close");

        assert_eq!(generation.active_references(RuntimeReferenceKind::Http1), 1);
        drop(lease);
        assert!(generation.drain(Duration::from_millis(100)));
    }

    #[test]
    fn failed_listener_candidate_releases_resources_and_preserves_active_generation() {
        let manager = GenerationManager::new();
        let active_candidate = manager.prepare(document()).expect("prepare active");
        let active = manager.activate(&active_candidate).expect("activate");
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied");
        let address = occupied.local_addr().expect("address");
        let mut candidate = active.config().to_draft();
        candidate.listeners.push(oxiroute_config::Listener {
            name: "live".into(),
            bind: oxiroute_config::ListenerBind::Socket { address },
            protocol: oxiroute_config::Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        candidate.rtmp_services.push(oxiroute_config::RtmpService {
            name: "live".into(),
            outbound_chunk_size: 4_096,
            max_inbound_message_size: 8 * 1024 * 1024,
            ack_window_size: 5_000_000,
            access_log: None,
            outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
            exec_profiles: Vec::new(),
            applications: vec![oxiroute_config::RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: false,
                publish: oxiroute_config::RtmpAccessPolicy::default(),
                play: oxiroute_config::RtmpAccessPolicy::default(),
                limits: oxiroute_config::RtmpSessionCeilings::default(),
                push_targets: Vec::new(),
                pull_targets: Vec::new(),
                relay: oxiroute_config::RtmpRelayPolicy::default(),
                callbacks: oxiroute_config::RtmpCallbackConfig::default(),
                fanout: oxiroute_config::RtmpFanoutPolicy::default(),
                vod: None,
                hls: None,
                dash: None,
                recorders: Vec::new(),
            }],
        });

        assert!(manager.prepare(document_for(&candidate)).is_err());
        assert!(Arc::ptr_eq(
            &manager.active().expect("still active"),
            &active
        ));
        assert!(active.accepting());
        assert!(manager.candidate().is_none());
    }

    #[test]
    fn superseded_candidate_cannot_activate_a_newer_preparation() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let second = manager.prepare(document()).expect("second candidate");

        assert!(matches!(
            manager.activate(&first),
            Err(GenerationError::CandidateSuperseded)
        ));
        let active = manager.activate(&second).expect("second activation");
        assert!(Arc::ptr_eq(&active, second.generation()));
    }

    #[test]
    fn publication_moves_the_admission_gate_without_overlap() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager.prepare(document()).expect("second candidate");

        assert!(first.generation().begin_admission().is_some());
        assert!(second.generation().begin_admission().is_none());
        manager.activate(&second).expect("second activation");
        assert!(first.generation().begin_admission().is_none());
        assert!(second.generation().begin_admission().is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publication_waits_for_acceptor_quiescence() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let second = manager.prepare(document()).expect("second candidate");
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = std::sync::mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            let result = activation_manager
                .activate(&activation_candidate)
                .map(|activated| Arc::ptr_eq(&activated, activation_candidate.generation()))
                .map_err(|error| error.code());
            activated_tx.send(result).expect("activation receiver");
        });

        let state = acceptor.changed().await.expect("gate close notification");
        assert!(!state.accepting);
        assert!(!second.generation().accepting());
        assert!(activated_rx.try_recv().is_err());

        acceptor.acknowledge(state.epoch);
        assert!(activated_rx.try_recv().is_err());
        drop(ownership);

        let activated_second = activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result")
            .expect("second activation");
        assert!(activated_second);
        assert!(second.generation().accepting());
        activation.join().expect("activation thread");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_remains_readable_on_a_single_thread_runtime_during_publication() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let (activation_paused, release_activation) =
            pause_activation_at(&manager, ActivationHookPoint::GateClosed);
        let reads = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let reader_manager = manager.clone();
        let reader_reads = Arc::clone(&reads);
        let reader_stop = Arc::clone(&stop);
        let reader = tokio::spawn(async move {
            while !reader_stop.load(Ordering::Acquire) {
                let _ = reader_manager.status();
                reader_reads.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        });
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });

        activation_paused
            .recv_timeout(Duration::from_secs(1))
            .expect("activation did not reach closed gate");
        let gate_state = tokio::time::timeout(Duration::from_secs(1), acceptor.changed())
            .await
            .expect("gate close timeout")
            .expect("gate close");
        let reads_at_close = reads.load(Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(1), async {
            while reads.load(Ordering::Relaxed) <= reads_at_close {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("status reader did not progress while publication was paused");
        let status = manager.status();
        assert_eq!(
            status.active_revision,
            Some(first.revision().candidate.clone())
        );
        assert_eq!(
            status.candidate_revision,
            Some(second.revision().candidate.clone())
        );
        assert!(!status.active_accepting);

        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);
        release_activation.wait();
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result")
            .expect("activation");
        stop.store(true, Ordering::Release);
        reader.await.expect("status reader");
        activation.join().expect("activation thread");
    }

    #[test]
    fn shutdown_close_before_activation_close_prevents_publication() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let (activation_paused, release_activation) =
            pause_activation_at(&manager, ActivationHookPoint::Reserved);
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });
        activation_paused
            .recv_timeout(Duration::from_secs(1))
            .expect("activation reservation pause");

        assert!(manager.shutdown(Duration::ZERO));
        release_activation.wait();

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::MutationInProgress.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(!first.generation().accepting());
        assert!(!second.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().activations, 1);
    }

    #[test]
    fn shutdown_during_closed_gate_prevents_activation_commit() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let (activation_paused, release_activation) =
            pause_activation_at(&manager, ActivationHookPoint::GateClosed);
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });
        activation_paused
            .recv_timeout(Duration::from_secs(1))
            .expect("closed gate pause");

        assert!(manager.shutdown(Duration::ZERO));
        release_activation.wait();

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::MutationInProgress.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(!first.generation().accepting());
        assert!(!second.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().activations, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_releases_manager_state_while_owned_generations_drain() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let shutdown_manager = manager.clone();
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let shutdown = thread::spawn(move || {
            shutdown_tx
                .send(shutdown_manager.shutdown(Duration::from_secs(1)))
                .expect("shutdown receiver");
        });

        let gate_state = tokio::time::timeout(Duration::from_secs(1), acceptor.changed())
            .await
            .expect("shutdown gate timeout")
            .expect("shutdown gate close");
        let status = manager.status();
        assert_eq!(
            status.active_revision,
            Some(first.revision().candidate.clone())
        );
        assert!(!status.active_accepting);
        assert!(status.candidate_revision.is_none());
        assert!(matches!(
            manager.activate(&second),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(second.generation().runtime_failed());

        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);
        assert!(
            shutdown_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("shutdown result")
        );
        shutdown.join().expect("shutdown thread");
        drop(startup);
    }

    #[test]
    fn terminal_shutdown_rejects_new_mutations_but_keeps_validation_read_only() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let active = manager.activate(&first).expect("first activation");
        let pending = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("pending candidate");

        drop(manager.begin_shutdown(Instant::now() + Duration::from_secs(1)));

        assert!(matches!(
            manager.begin_mutation(&active.revision().candidate),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.begin_config_mutation(),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.begin_candidate_start(&pending),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.prepare(document()),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(matches!(
            manager.rollback(),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(manager.validate_candidate(document()).is_ok());
    }

    #[test]
    fn repeated_shutdown_retains_active_previous_and_removed_candidate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");

        let first_shutdown = manager.reserve_shutdown();
        let second_shutdown = manager.reserve_shutdown();

        assert_eq!(first_shutdown.len(), 3);
        assert_eq!(second_shutdown.len(), 3);
        for expected in [second.generation(), first.generation(), third.generation()] {
            assert!(
                first_shutdown
                    .iter()
                    .any(|generation| Arc::ptr_eq(generation, expected))
            );
            assert!(
                second_shutdown
                    .iter()
                    .any(|generation| Arc::ptr_eq(generation, expected))
            );
        }
        assert!(manager.candidate().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn candidate_can_be_superseded_while_the_gate_is_quiescing() {
        let manager = GenerationManager::new();
        let first = manager
            .prepare(document_with_max_connections(Some(1)))
            .expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");
        assert!(candidate_matches(manager.candidate().as_ref(), &third));
        assert!(matches!(
            manager.activate(&third),
            Err(GenerationError::MutationInProgress)
        ));
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::CandidateSuperseded.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(!second.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &third));
        assert_eq!(manager.status().failures, 0);
        drop(acceptor);
        let active = manager.activate(&third).expect("third activation");
        assert!(Arc::ptr_eq(&active, third.generation()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_activation_is_rejected_without_touching_the_gate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let activation_manager = manager.clone();
        let activation_candidate = second.clone();
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(
                    activation_manager
                        .activate(&activation_candidate)
                        .map(|_| ())
                        .map_err(|error| error.code()),
                )
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        assert!(matches!(
            manager.activate(&second),
            Err(GenerationError::MutationInProgress)
        ));
        assert!(activated_rx.try_recv().is_err());
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);
        activated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("activation result")
            .expect("activation");
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            second.generation()
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quarantine_during_publication_cancels_once_and_reopens_the_active_gate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(startup.activate().map(|_| ()).map_err(|error| error.code()))
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        manager.quarantine(&second, "runtime_start");
        manager.quarantine(&second, "runtime_start");
        assert_eq!(manager.status().failures, 1);
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::CandidateSuperseded.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().failures, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_failure_during_publication_is_quarantined_without_retry() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let mut acceptor = first.generation().accept_gate().register();
        let ownership = acceptor.claim().expect("accept ownership");
        let (activated_tx, activated_rx) = mpsc::sync_channel(1);
        let activation = thread::spawn(move || {
            activated_tx
                .send(startup.activate().map(|_| ()).map_err(|error| error.code()))
                .expect("activation receiver");
        });

        let gate_state = acceptor.changed().await.expect("gate close");
        second.generation().mark_runtime_failed();
        acceptor.acknowledge(gate_state.epoch);
        drop(ownership);

        assert_eq!(
            activated_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("activation result"),
            Err(GenerationError::RuntimePrepare.code())
        );
        activation.join().expect("activation thread");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().failures, 1);
        assert!(matches!(
            manager.begin_candidate_start(&second),
            Err(GenerationError::CandidateSuperseded)
        ));
    }

    #[test]
    fn supersession_precedes_runtime_failure_and_quiesce_timeout() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let reservation = manager
            .reserve_activation(&second, None)
            .expect("activation reservation");
        let close = first.generation().accept_gate().close();
        let third = manager
            .prepare(document_with_max_connections(Some(3)))
            .expect("third candidate");
        second.generation().mark_runtime_failed();

        assert!(matches!(
            manager.publish_reserved_candidate(&second, &reservation, Some(close), false),
            Err(GenerationError::CandidateSuperseded)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &third));
        assert_eq!(manager.status().failures, 0);
    }

    #[test]
    fn runtime_failure_precedes_quiesce_timeout_and_uses_runtime_start_quarantine() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);
        let reservation = manager
            .reserve_activation(&second, Some(startup.reservation_token))
            .expect("activation reservation");
        let close = first.generation().accept_gate().close();
        second.generation().mark_runtime_failed();

        assert!(matches!(
            manager.publish_reserved_candidate(&second, &reservation, Some(close), false),
            Err(GenerationError::RuntimePrepare)
        ));
        startup.completed = true;
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        let status = manager.status();
        assert_eq!(status.failures, 1);
        assert_eq!(status.last_failure, Some("runtime_start"));
    }

    #[test]
    fn gate_timeout_consumes_a_started_candidate_and_reopens_the_active_generation() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let ownership = first
            .generation()
            .begin_admission()
            .expect("accept ownership");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("startup reservation");
        claim_and_mark_runtime_started(&mut startup);

        assert!(matches!(
            startup.activate_with_timeout(Duration::ZERO),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(first.generation().accepting());
        assert!(manager.candidate().is_none());
        let status = manager.status();
        assert_eq!(status.failures, 1);
        assert_eq!(
            status.quarantined_revision,
            Some(second.revision().candidate.clone())
        );
        assert!(matches!(
            manager.begin_candidate_start(&second),
            Err(GenerationError::CandidateSuperseded)
        ));
        drop(ownership);
    }

    #[test]
    fn gate_timeout_keeps_an_unstarted_candidate_retryable() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let ownership = first
            .generation()
            .begin_admission()
            .expect("accept ownership");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");

        assert!(matches!(
            manager.activate_inner(&second, None, Duration::ZERO),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(candidate_matches(manager.candidate().as_ref(), &second));
        assert!(first.generation().accepting());
        assert_eq!(manager.status().failures, 0);
        drop(ownership);

        let active = manager
            .activate(&second)
            .expect("retry unstarted candidate");
        assert!(Arc::ptr_eq(&active, second.generation()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_publication_does_not_reopen_an_already_closed_active_gate() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut acceptor = first.generation().accept_gate().register();
        first.generation().stop_accepting();
        assert!(!first.generation().accepting());
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");

        assert!(matches!(
            manager.activate_inner(&second, None, Duration::ZERO),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(!first.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &second));
        let gate_state = acceptor.changed().await.expect("gate close");
        acceptor.acknowledge(gate_state.epoch);
    }

    #[test]
    fn publication_failure_does_not_undo_a_later_shutdown_close() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let reservation = manager
            .reserve_activation(&second, None)
            .expect("activation reservation");
        let activation_close = first.generation().accept_gate().close();
        first.generation().stop_accepting();

        assert!(matches!(
            manager.publish_reserved_candidate(
                &second,
                &reservation,
                Some(activation_close),
                false,
            ),
            Err(GenerationError::AcceptorQuiesce)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("active"),
            first.generation()
        ));
        assert!(!first.generation().accepting());
        assert!(candidate_matches(manager.candidate().as_ref(), &second));
    }

    #[test]
    fn cancelling_a_claimed_start_rejects_a_late_marker_and_consumes_the_candidate_once() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("candidate");
        let mut startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");
        let generation = startup.claim_runtime_start().expect("runtime start claim");
        assert!(matches!(
            startup.claim_runtime_start(),
            Err(GenerationError::CandidateSuperseded)
        ));
        let marker_generation = Arc::clone(&generation);
        let (marker_ready, start_marker) = mpsc::sync_channel(0);
        let marker = thread::spawn(move || {
            start_marker.recv().expect("marker release");
            marker_generation.mark_runtime_started()
        });

        drop(startup);
        marker_ready.send(()).expect("marker receiver");

        assert!(!marker.join().expect("runtime marker"));
        assert!(generation.runtime_failed());
        assert!(!generation.claim_runtime_start());
        assert!(manager.candidate().is_none());
        assert_eq!(manager.status().failures, 1);
        manager.quarantine(&candidate, "runtime_start");
        assert_eq!(manager.status().failures, 1);
        assert!(matches!(
            manager.begin_candidate_start(&candidate),
            Err(GenerationError::CandidateSuperseded)
        ));
    }

    #[test]
    fn activation_before_runtime_claim_releases_only_the_unstarted_reservation() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("candidate");
        let startup = manager
            .begin_candidate_start(&candidate)
            .expect("startup reservation");

        assert!(matches!(
            startup.activate(),
            Err(GenerationError::RuntimePrepare)
        ));
        assert!(candidate_matches(manager.candidate().as_ref(), &candidate));
        assert!(!candidate.generation().runtime_failed());
        assert_eq!(manager.status().failures, 0);

        let mut next_startup = manager
            .begin_candidate_start(&candidate)
            .expect("next startup reservation");
        claim_and_mark_runtime_started(&mut next_startup);
        let active = next_startup.activate().expect("activation");
        assert!(Arc::ptr_eq(&active, candidate.generation()));
    }

    #[test]
    fn stale_startup_token_cannot_cancel_a_new_reservation() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let active = manager.activate(&first).expect("first activation");
        let second = manager
            .prepare(document_with_max_connections(Some(2)))
            .expect("second candidate");
        let stale = manager
            .begin_candidate_start(&second)
            .expect("first startup reservation");
        let stale_token = stale.reservation_token;
        drop(stale);
        let mut current = manager
            .begin_candidate_start(&second)
            .expect("second startup reservation");

        manager.cancel_startup(&second, stale_token);
        assert!(matches!(
            manager.begin_mutation(&active.revision().candidate),
            Err(GenerationError::MutationInProgress)
        ));
        claim_and_mark_runtime_started(&mut current);
        let activated = current.activate().expect("current startup activation");
        assert!(Arc::ptr_eq(&activated, second.generation()));
    }

    #[test]
    fn quarantined_candidate_leaves_active_unchanged_and_can_be_prepared_again() {
        let manager = GenerationManager::new();
        let active_candidate = manager.prepare(document()).expect("active candidate");
        let active = manager.activate(&active_candidate).expect("active");
        let failed = manager.prepare(document()).expect("failed candidate");

        manager.quarantine(&failed, "runtime_start");
        assert!(Arc::ptr_eq(
            &manager.active().expect("active retained"),
            &active
        ));
        assert!(active.accepting());
        assert!(manager.status().quarantined_revision.is_some());
        assert!(manager.prepare(document()).is_ok());
    }

    #[test]
    fn failed_rollback_revision_cannot_be_started_again() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        manager.activate(&first).expect("first activation");
        let mut second_config = first.generation().config.to_draft();
        second_config.max_connections = Some(2);
        let second = manager
            .prepare(document_for(&second_config))
            .expect("second candidate");
        manager.activate(&second).expect("second activation");

        let rollback = manager.rollback().expect("rollback candidate");
        manager.quarantine(&rollback, "runtime_start");
        assert!(matches!(
            manager.rollback(),
            Err(GenerationError::QuarantinedRevision)
        ));
        assert!(Arc::ptr_eq(
            &manager.active().expect("second remains active"),
            second.generation()
        ));
    }

    #[test]
    fn stale_revision_cannot_acquire_a_mutation_permit() {
        let manager = GenerationManager::new();
        let candidate = manager.prepare(document()).expect("candidate");
        let active = manager.activate(&candidate).expect("active");
        let stale = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            .parse()
            .unwrap();

        assert!(matches!(
            manager.begin_mutation(&stale),
            Err(GenerationError::RevisionConflict)
        ));
        assert!(manager.begin_mutation(&active.revision().candidate).is_ok());
    }

    #[test]
    fn candidate_start_waits_for_mutation_and_then_excludes_new_mutations_until_publication() {
        let manager = GenerationManager::new();
        let first = manager.prepare(document()).expect("first candidate");
        let active = manager.activate(&first).expect("active");
        let second = manager.prepare(document()).expect("second candidate");
        let mutation = manager
            .begin_mutation(&active.revision().candidate)
            .expect("active mutation");

        assert!(matches!(
            manager.begin_candidate_start(&second),
            Err(GenerationError::MutationInProgress)
        ));
        drop(mutation);

        let mut startup = manager
            .begin_candidate_start(&second)
            .expect("reserved candidate startup");
        assert!(matches!(
            manager.begin_mutation(&active.revision().candidate),
            Err(GenerationError::MutationInProgress)
        ));
        claim_and_mark_runtime_started(&mut startup);
        let activated = startup.activate().expect("reserved activation");
        assert!(Arc::ptr_eq(&activated, second.generation()));
    }
}
