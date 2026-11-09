#[allow(clippy::wildcard_imports)]
use super::*;

pub struct RuntimeGeneration {
    pub(super) accept_gate: AcceptGate,
    pub(super) config: Arc<ValidatedConfig>,
    /// The mutex counts the threads blocked in `drain`, so releasing a reference only notifies when one waits.
    pub(super) drain: (Mutex<usize>, Condvar),
    pub(super) metrics: RuntimeMetrics,
    inventory: ListenerInventory,
    resources: GenerationResources,
    pub(super) references: [AtomicU64; 9],
    pub(super) mutations: AtomicU64,
    pub(super) revision: GenerationRevision,
    runtime_lifecycle: AtomicU8,
    runtime_failed: AtomicBool,
    /// What the previous generation's operators changed, kept to re-apply to registered servers
    /// that only join this generation's pools at activation.
    carried_states: Mutex<Vec<ServerAdminState>>,
    retired: tokio::sync::watch::Sender<bool>,
}

const RUNTIME_PREPARED: u8 = 0;
const RUNTIME_START_CLAIMED: u8 = 1;
const RUNTIME_STARTED: u8 = 2;
const RUNTIME_CANCELLED: u8 = 3;

impl RuntimeGeneration {
    pub(super) fn activate(prepared: PreparedGeneration) -> Self {
        Self {
            accept_gate: AcceptGate::closed(),
            config: prepared.config,
            drain: (Mutex::new(0), Condvar::new()),
            metrics: prepared.metrics,
            inventory: prepared.inventory,
            resources: prepared.resources,
            references: std::array::from_fn(|_| AtomicU64::new(0)),
            mutations: AtomicU64::new(0),
            revision: prepared.revision,
            runtime_lifecycle: AtomicU8::new(RUNTIME_PREPARED),
            runtime_failed: AtomicBool::new(false),
            carried_states: Mutex::new(Vec::new()),
            retired: tokio::sync::watch::channel(false).0,
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
    pub fn administrative_states(&self) -> Vec<ServerAdminState> {
        self.pools()
            .iter()
            .flat_map(|pool| pool.administrative_states())
            .collect()
    }

    pub fn restore_administrative_states(&self, states: &[ServerAdminState]) {
        for pool in self.pools() {
            pool.restore_administrative_states(states);
        }
        *self
            .carried_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = states.to_vec();
    }

    /// Applies the carried states again, to servers that joined the pools after the first pass.
    pub(crate) fn restore_carried_states(&self) {
        let states = self
            .carried_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for pool in self.pools() {
            pool.restore_administrative_states(&states);
        }
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

    pub(crate) fn listener_restart_required(&self, candidate: &ValidatedConfig) -> bool {
        self.inventory
            .restart_required(&ListenerInventory::compile(candidate))
    }

    /// Describes what activating `candidate` would do to the listener sockets this generation owns.
    pub(crate) fn listener_changes(
        &self,
        candidate: &ValidatedConfig,
    ) -> Option<crate::listener::inventory::ListenerChangeSummary> {
        crate::listener::reservation::listener_changes(
            &self.inventory,
            &ListenerInventory::compile(candidate),
        )
    }

    /// Whether `candidate` needs other sockets than this generation owns.
    ///
    /// A supervised worker only adopts sockets, so only the supervising master can apply it.
    pub(crate) fn listener_set_changed(&self, candidate: &ValidatedConfig) -> bool {
        !self
            .inventory
            .same_descriptor_topology(&ListenerInventory::compile(candidate))
    }

    pub(crate) fn listener_restart_reason(
        &self,
        candidate: &ValidatedConfig,
    ) -> Option<crate::listener::inventory::ListenerRestartReason> {
        self.inventory
            .restart_reason(&ListenerInventory::compile(candidate))
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

    pub(super) fn rtmp_retirement(&self) -> RtmpRetirement {
        self.resources.rtmp_retirement()
    }

    pub fn rtmp_shutdown(&self) -> RtmpShutdown {
        self.resources.rtmp_shutdown()
    }

    pub(super) fn claim_runtime_start(&self) -> bool {
        self.runtime_lifecycle
            .compare_exchange(
                RUNTIME_PREPARED,
                RUNTIME_START_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn start_rtmp(&self) -> Result<(), GenerationError> {
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

    pub(super) fn cancel_runtime_start(&self) {
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
            self.wake_drain();
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

    pub(super) fn start_accepting(&self) {
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

    /// How long work admitted by this generation may continue after it is replaced.
    #[must_use]
    pub fn retirement_deadline(&self) -> Duration {
        Duration::from_millis(self.config.retirement_deadline_ms())
    }

    /// Reports that this generation is being closed before its work ended; `reason` is
    /// `drain_deadline` or `shutdown`.
    pub fn announce_forced_close(&self, reason: &str) {
        log::warn!(
            "retired generation {} closed by {reason} with {} requests in flight",
            self.revision().candidate.as_str(),
            self.in_flight()
        );
        crate::observability::operational_event::emit_retire_forced_close(
            &self.revision().candidate,
            reason,
        );
    }

    /// Whether a newer generation replaced this one. Unlike a closed accept gate, this never
    /// reverts, and an operator drain of the active generation does not set it.
    #[must_use]
    pub fn retired(&self) -> bool {
        *self.retired.borrow()
    }

    /// Records that a newer generation replaced this one; idle HTTP/1 connections end and busy
    /// ones close after their response.
    pub fn mark_retired(&self) {
        self.retired.send_replace(true);
    }

    /// Resolves with `true` once this generation is replaced.
    #[must_use]
    pub fn retirement_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.retired.subscribe()
    }

    /// Connections and streams admitted by this generation that have not ended.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.references
            .iter()
            .map(|counter| counter.load(Ordering::Acquire))
            .sum()
    }

    #[must_use]
    pub fn in_flight_by_kind(&self) -> BTreeMap<&'static str, u64> {
        RuntimeReferenceKind::ALL
            .into_iter()
            .map(|kind| (kind.label(), self.active_references(kind)))
            .filter(|&(_, count)| count != 0)
            .collect()
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
        let mut waiters = self
            .drain
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *waiters += 1;
        let drained = loop {
            let drained = self.drained();
            let now = Instant::now();
            if drained || now >= deadline {
                break drained;
            }
            let (next, wait) = self
                .drain
                .1
                .wait_timeout(waiters, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            waiters = next;
            if wait.timed_out() {
                break self.drained();
            }
        };
        *waiters -= 1;
        drained
    }

    pub(super) fn wake_drain(&self) {
        let waiters = self
            .drain
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *waiters > 0 {
            self.drain.1.notify_all();
        }
    }
}
