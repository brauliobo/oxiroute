#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub struct RuntimeMetrics {
    pub(super) inner: Arc<RuntimeMetricsInner>,
}

/// Process execution mode reported by the management capability surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Direct,
    Supervised,
}

pub(super) struct RuntimeMetricsInner {
    pub(super) process_admission: Arc<ProcessAdmissionState>,
    pub(super) process_runtime: ProcessRuntime,
    pub(super) listeners: RwLock<ListenerMetricsRegistry>,
    pub(super) upstream_pools: RwLock<Vec<Arc<RoundRobinPool>>>,
    pub(super) certbot: RwLock<CertbotMonitoring>,
    pub(super) acme_managed: RwLock<AcmeManagedMonitoring>,
    pub(super) direct_files: RwLock<DirectFileMonitoring>,
    pub(super) previous_cpu_sample: Mutex<Option<CpuSample>>,
    rtmp_recording_supported: AtomicBool,
    pub(super) generation_started_at: Instant,
}

#[derive(Clone)]
pub struct ProcessRuntime {
    pub(super) inner: Arc<ProcessRuntimeInner>,
}

pub(super) struct ProcessRuntimeInner {
    admission: Arc<ProcessAdmissionState>,
    listeners: Mutex<HashMap<ListenerId, ProcessListenerMetricsEntry>>,
    next_listener_transaction: AtomicU64,
    pub(super) transport_operations: Arc<TransportOperationsState>,
    access_records: Arc<Mutex<VecDeque<AccessRecord>>>,
    mode: RuntimeMode,
    pub(super) started_at: Instant,
}

impl ProcessRuntime {
    #[must_use]
    pub fn new(max_connections: Option<u64>) -> Self {
        Self::with_mode(max_connections, RuntimeMode::Direct)
    }

    /// Creates process metrics for a worker using authenticated listener adoption.
    #[must_use]
    pub fn supervised(max_connections: Option<u64>) -> Self {
        Self::with_mode(max_connections, RuntimeMode::Supervised)
    }

    fn with_mode(max_connections: Option<u64>, mode: RuntimeMode) -> Self {
        Self {
            inner: Arc::new(ProcessRuntimeInner {
                admission: Arc::new(ProcessAdmissionState::new(max_connections)),
                listeners: Mutex::new(HashMap::new()),
                next_listener_transaction: AtomicU64::new(1),
                transport_operations: Arc::new(TransportOperationsState::new()),
                access_records: Arc::new(Mutex::new(VecDeque::with_capacity(
                    ACCESS_RECORD_CAPACITY,
                ))),
                mode,
                started_at: Instant::now(),
            }),
        }
    }

    fn listener(
        &self,
        id: &ListenerId,
        max_connections: Option<u64>,
    ) -> Result<Arc<SharedListenerMetricsState>, MetricsError> {
        let mut listeners = self
            .inner
            .listeners
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("process listeners"))?;
        Ok(Arc::clone(
            &listeners
                .entry(id.clone())
                .or_insert_with(|| ProcessListenerMetricsEntry {
                    state: Arc::new(SharedListenerMetricsState::new(
                        max_connections,
                        self.transport_operations(),
                        Arc::clone(&self.inner.access_records),
                    )),
                    committed: true,
                    reservations: HashSet::new(),
                })
                .state,
        ))
    }

    fn activate_limit(&self, max_connections: Option<u64>) {
        self.inner.admission.set_limit(max_connections);
    }

    fn transport_operations(&self) -> Arc<TransportOperationsState> {
        Arc::clone(&self.inner.transport_operations)
    }

    pub(super) fn access_records_snapshot(&self) -> Result<Vec<AccessRecord>, MetricsError> {
        Ok(self
            .inner
            .access_records
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("access records"))?
            .iter()
            .cloned()
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn listener_count(&self) -> usize {
        self.inner
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn mode(&self) -> RuntimeMode {
        self.inner.mode
    }
}

pub(super) fn append_access_record(records: &Mutex<VecDeque<AccessRecord>>, record: AccessRecord) {
    let mut records = records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if records.len() == ACCESS_RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record.clone());
    drop(records);
    if let Ok(value) = serde_json::to_value(record) {
        crate::observability::logging::log_json("oxiroute::access", &value);
    }
}

#[derive(Default)]
pub(super) struct CertbotMonitoring {
    pub(super) reconcilers: Vec<Arc<CertbotReconciler>>,
    pub(super) watcher: Option<CertbotWatcherMonitor>,
}

#[derive(Default)]
pub(super) struct DirectFileMonitoring {
    pub(super) reconcilers: Vec<Arc<FileReconciler>>,
    pub(super) watcher: Option<FileWatcherMonitor>,
}

#[derive(Default)]
pub(super) struct AcmeManagedMonitoring {
    pub(super) reconcilers: Vec<Arc<AcmeManagedReconciler>>,
}

impl RuntimeMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_connections(None)
    }

    #[must_use]
    pub fn with_max_connections(max_connections: Option<u64>) -> Self {
        Self::for_process(ProcessRuntime::new(max_connections))
    }

    #[must_use]
    pub fn for_process(process_runtime: ProcessRuntime) -> Self {
        Self {
            inner: Arc::new(RuntimeMetricsInner {
                process_admission: Arc::clone(&process_runtime.inner.admission),
                process_runtime,
                listeners: RwLock::new(ListenerMetricsRegistry::default()),
                upstream_pools: RwLock::new(Vec::new()),
                certbot: RwLock::new(CertbotMonitoring::default()),
                acme_managed: RwLock::new(AcmeManagedMonitoring::default()),
                direct_files: RwLock::new(DirectFileMonitoring::default()),
                previous_cpu_sample: Mutex::new(None),
                rtmp_recording_supported: AtomicBool::new(false),
                generation_started_at: Instant::now(),
            }),
        }
    }

    /// Returns whether this process is serving directly or through the supervisor.
    #[must_use]
    pub fn supervision_mode(&self) -> RuntimeMode {
        self.inner.process_runtime.mode()
    }

    pub(crate) fn activate_limits(&self, max_connections: Option<u64>) {
        self.inner.process_runtime.activate_limit(max_connections);
        if let Ok(listeners) = self.inner.listeners.read() {
            for listener in listeners.states.values() {
                listener.shared.set_limit(listener.max_connections);
            }
        }
    }

    /// Registers the immutable upstream pools in canonical definition order.
    ///
    /// # Errors
    ///
    /// Returns an error when the pool registry is poisoned.
    pub fn register_upstream_pools(
        &self,
        pools: impl IntoIterator<Item = Arc<RoundRobinPool>>,
    ) -> Result<(), MetricsError> {
        let mut registered = self
            .inner
            .upstream_pools
            .write()
            .map_err(|_| MetricsError::StatePoisoned("upstream pools"))?;
        *registered = pools.into_iter().collect();
        Ok(())
    }

    /// Registers the process-lifetime Certbot reconcilers and watcher monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when the Certbot monitoring registry is poisoned.
    pub fn register_certbot_monitoring(
        &self,
        reconcilers: impl IntoIterator<Item = Arc<CertbotReconciler>>,
        watcher: Option<CertbotWatcherMonitor>,
    ) -> Result<(), MetricsError> {
        let mut certbot = self
            .inner
            .certbot
            .write()
            .map_err(|_| MetricsError::StatePoisoned("Certbot monitoring"))?;
        certbot.reconcilers = reconcilers.into_iter().collect();
        certbot.watcher = watcher;
        Ok(())
    }

    /// Registers the managed ACME reconcilers for status, API, and Prometheus snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed ACME monitoring registry is poisoned.
    pub fn register_acme_managed_monitoring(
        &self,
        reconcilers: impl IntoIterator<Item = Arc<AcmeManagedReconciler>>,
    ) -> Result<(), MetricsError> {
        let mut managed = self
            .inner
            .acme_managed
            .write()
            .map_err(|_| MetricsError::StatePoisoned("managed ACME monitoring"))?;
        managed.reconcilers = reconcilers.into_iter().collect();
        Ok(())
    }

    /// Registers the process-lifetime direct-file reconcilers and watcher monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when the direct-file monitoring registry is poisoned.
    pub fn register_direct_file_monitoring(
        &self,
        reconcilers: impl IntoIterator<Item = Arc<FileReconciler>>,
        watcher: Option<FileWatcherMonitor>,
    ) -> Result<(), MetricsError> {
        let mut direct_files = self
            .inner
            .direct_files
            .write()
            .map_err(|_| MetricsError::StatePoisoned("direct-file monitoring"))?;
        direct_files.reconcilers = reconcilers.into_iter().collect();
        direct_files.watcher = watcher;
        Ok(())
    }

    /// Registers a listener and returns its accounting handle.
    ///
    /// # Errors
    ///
    /// Returns an error when any field is empty, the listener name is already registered, or the
    /// listener registry is poisoned.
    pub fn register_listener(
        &self,
        name: impl Into<String>,
        protocol: impl Into<String>,
        bind: impl Into<String>,
        max_connections: impl Into<Option<u64>>,
    ) -> Result<ListenerMetrics, MetricsError> {
        let name = name.into();
        self.register_listener_with_policy(
            ListenerId::Legacy(name.clone()),
            name,
            protocol.into(),
            bind.into(),
            max_connections.into(),
            ListenerMetricPolicy::Public,
        )
    }

    /// Registers a canonical listener with a stable, transport-qualified bind identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the Unix socket path cannot be represented without loss, a field is
    /// invalid, the listener name is already registered, or the listener registry is poisoned.
    pub fn register_configured_listener(
        &self,
        name: impl Into<String>,
        protocol: impl Into<String>,
        bind: &ListenerBind,
        max_connections: Option<u64>,
    ) -> Result<ListenerMetrics, MetricsError> {
        let name = name.into();
        let bind = match bind {
            ListenerBind::Socket { address, .. } => format!("socket:{address}"),
            ListenerBind::Udp { address } => format!("udp:{address}"),
            ListenerBind::Unix { path, .. } => {
                let path = path
                    .to_str()
                    .ok_or_else(|| MetricsError::InvalidListenerBind {
                        listener: name.clone(),
                        detail: "Unix socket path is not valid UTF-8",
                    })?;
                format!("unix:{path}")
            }
        };
        self.register_listener_with_policy(
            ListenerId::Legacy(name.clone()),
            name,
            protocol.into(),
            bind,
            max_connections,
            ListenerMetricPolicy::Public,
        )
    }

    pub(crate) fn register_inventory(
        &self,
        entries: &[ListenerEntry],
    ) -> Result<ListenerRegistrationTransaction, MetricsError> {
        let prepared = entries
            .iter()
            .map(|entry| {
                Ok((
                    entry.id.clone(),
                    entry.name.clone(),
                    entry.protocol_name().to_owned(),
                    configured_bind_identity(&entry.name, &entry.bind)?,
                    entry.max_connections,
                    entry.metric_policy,
                ))
            })
            .collect::<Result<Vec<_>, MetricsError>>()?;
        let mut process_listeners = self
            .inner
            .process_runtime
            .inner
            .listeners
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("process listeners"))?;
        let mut listeners = self
            .inner
            .listeners
            .write()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        let mut ids = HashSet::with_capacity(prepared.len());
        for (id, name, ..) in &prepared {
            if !ids.insert(id) || listeners.states.contains_key(id) {
                return Err(MetricsError::DuplicateListener(name.clone()));
            }
        }
        let token = self
            .inner
            .process_runtime
            .inner
            .next_listener_transaction
            .fetch_add(1, Ordering::Relaxed);
        for (id, name, protocol, bind, max_connections, metric_policy) in prepared {
            let process_entry = process_listeners.entry(id.clone()).or_insert_with(|| {
                ProcessListenerMetricsEntry {
                    state: Arc::new(SharedListenerMetricsState::new(
                        max_connections,
                        self.inner.process_runtime.transport_operations(),
                        Arc::clone(&self.inner.process_runtime.inner.access_records),
                    )),
                    committed: false,
                    reservations: HashSet::new(),
                }
            });
            process_entry.reservations.insert(token);
            let shared = Arc::clone(&process_entry.state);
            listeners.order.push(id.clone());
            listeners.states.insert(
                id,
                Arc::new(ListenerMetricsState::new(
                    name,
                    protocol,
                    bind,
                    max_connections,
                    metric_policy,
                    shared,
                )),
            );
        }
        Ok(ListenerRegistrationTransaction {
            process: self.inner.process_runtime.clone(),
            ids: entries.iter().map(|entry| entry.id.clone()).collect(),
            token,
            committed: false,
        })
    }

    pub(super) fn register_listener_with_policy(
        &self,
        id: ListenerId,
        name: String,
        protocol: String,
        bind: String,
        max_connections: Option<u64>,
        metric_policy: ListenerMetricPolicy,
    ) -> Result<ListenerMetrics, MetricsError> {
        validate_listener_field("name", &name)?;
        validate_listener_field("protocol", &protocol)?;
        validate_listener_field("bind", &bind)?;
        if max_connections == Some(0) {
            return Err(MetricsError::InvalidListenerField("max_connections"));
        }

        let mut listeners = self
            .inner
            .listeners
            .write()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        match listeners.states.entry(id.clone()) {
            Entry::Vacant(entry) => {
                let process_id = match &id {
                    ListenerId::Legacy(_) => ListenerId::Legacy(bind.clone()),
                    _ => id.clone(),
                };
                let shared = self
                    .inner
                    .process_runtime
                    .listener(&process_id, max_connections)?;
                let state = Arc::new(ListenerMetricsState::new(
                    name,
                    protocol,
                    bind,
                    max_connections,
                    metric_policy,
                    shared,
                ));
                entry.insert(Arc::clone(&state));
                listeners.order.push(id);
                Ok(ListenerMetrics {
                    process: Arc::clone(&self.inner.process_admission),
                    state,
                })
            }
            Entry::Occupied(_) => Err(MetricsError::DuplicateListener(name)),
        }
    }

    /// Returns the accounting handle for a registered listener.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener registry is poisoned.
    pub fn listener(&self, name: &str) -> Result<Option<ListenerMetrics>, MetricsError> {
        self.listener_by_id(&ListenerId::Legacy(name.to_owned()))
    }

    pub(crate) fn inventory_listener(
        &self,
        id: &ListenerId,
    ) -> Result<Option<ListenerMetrics>, MetricsError> {
        self.listener_by_id(id)
    }

    fn listener_by_id(&self, id: &ListenerId) -> Result<Option<ListenerMetrics>, MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        Ok(listeners.states.get(id).map(|state| ListenerMetrics {
            process: Arc::clone(&self.inner.process_admission),
            state: Arc::clone(state),
        }))
    }

    /// Accounts for a newly accepted connection on a named listener.
    ///
    /// The returned guard decrements the active connection count when dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener is unknown, the registry is poisoned, a configured
    /// connection cap is reached, or a counter would overflow.
    pub fn begin_connection(&self, listener_name: &str) -> Result<ConnectionGuard, MetricsError> {
        self.listener(listener_name)?
            .ok_or_else(|| MetricsError::ListenerNotFound(listener_name.to_owned()))?
            .begin_connection()
    }

    /// Acquires only process-wide connection capacity, for runtime-owned listeners without a
    /// canonical listener metrics identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cap is reached or an admission counter would overflow.
    pub fn begin_process_connection(&self) -> Result<ProcessConnectionGuard, MetricsError> {
        self.inner.process_admission.acquire()
    }

    /// Acquires process capacity for a local control-plane connection while bypassing a data-plane
    /// administrative drain.
    ///
    /// # Errors
    ///
    /// Returns an error when the process capacity is reached or a counter would overflow.
    pub fn begin_control_connection(&self) -> Result<ProcessConnectionGuard, MetricsError> {
        self.inner.process_admission.acquire_control()
    }

    pub fn set_process_administrative_state(&self, state: AdministrativeState) {
        self.inner
            .process_admission
            .administrative_state
            .store(state as u8, Ordering::Release);
    }

    /// Changes admission state for one exact listener.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener is unknown or the registry is poisoned.
    pub fn set_listener_administrative_state(
        &self,
        name: &str,
        state: AdministrativeState,
    ) -> Result<(), MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        let listener = listeners
            .order
            .iter()
            .filter_map(|id| listeners.states.get(id).map(|state| (id, state)))
            .find(|(id, listener)| {
                &*listener.name == name
                    && listener.metric_policy == ListenerMetricPolicy::Public
                    && matches!(id, ListenerId::Traffic(_) | ListenerId::Legacy(_))
            })
            .map(|(_, listener)| listener)
            .or_else(|| {
                listeners
                    .order
                    .iter()
                    .filter_map(|id| listeners.states.get(id))
                    .find(|listener| {
                        &*listener.name == name
                            && listener.metric_policy == ListenerMetricPolicy::Public
                    })
            })
            .ok_or_else(|| MetricsError::ListenerNotFound(name.to_owned()))?;
        listener
            .shared
            .administrative_state
            .store(state as u8, Ordering::Release);
        Ok(())
    }

    /// Records whether successfully activated RTMP services own any recorder runtime.
    pub fn set_rtmp_recording_supported(&self, supported: bool) {
        self.inner
            .rtmp_recording_supported
            .store(supported, Ordering::Release);
    }

    #[must_use]
    pub(crate) fn rtmp_recording_supported(&self) -> bool {
        self.inner.rtmp_recording_supported.load(Ordering::Acquire)
    }

    /// Records one bounded transport outcome and latency sample.
    ///
    /// The transport and outcome values are closed enums so this registry cannot grow with a
    /// request URI, stream name, endpoint, or other unbounded request data.
    ///
    /// # Errors
    ///
    /// Returns an error if a counter or latency total would overflow.
    pub fn record_transport_operation(
        &self,
        transport: ObservedTransport,
        outcome: TransportOutcome,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.inner
            .process_runtime
            .inner
            .transport_operations
            .record(transport, outcome, Some(duration))
    }
}

#[derive(Default)]
pub(super) struct ListenerMetricsRegistry {
    pub(super) states: HashMap<ListenerId, Arc<ListenerMetricsState>>,
    pub(super) order: Vec<ListenerId>,
}

struct ProcessListenerMetricsEntry {
    state: Arc<SharedListenerMetricsState>,
    committed: bool,
    reservations: HashSet<u64>,
}

#[must_use = "listener registration rolls back unless committed"]
pub(crate) struct ListenerRegistrationTransaction {
    process: ProcessRuntime,
    ids: Vec<ListenerId>,
    token: u64,
    committed: bool,
}

impl ListenerRegistrationTransaction {
    pub(crate) fn commit(mut self) -> Result<(), MetricsError> {
        let mut listeners = self
            .process
            .inner
            .listeners
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("process listeners"))?;
        for id in &self.ids {
            let entry = listeners
                .get_mut(id)
                .expect("listener transaction references process state");
            entry.reservations.remove(&self.token);
            entry.committed = true;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for ListenerRegistrationTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let Ok(mut listeners) = self.process.inner.listeners.lock() else {
            return;
        };
        for id in &self.ids {
            let remove = listeners.get_mut(id).is_some_and(|entry| {
                entry.reservations.remove(&self.token);
                !entry.committed && entry.reservations.is_empty()
            });
            if remove {
                listeners.remove(id);
            }
        }
    }
}

fn configured_bind_identity(name: &str, bind: &ListenerBind) -> Result<String, MetricsError> {
    match bind {
        ListenerBind::Socket { address, .. } => Ok(format!("socket:{address}")),
        ListenerBind::Udp { address } => Ok(format!("udp:{address}")),
        ListenerBind::Unix { path, .. } => path
            .to_str()
            .map(|path| format!("unix:{path}"))
            .ok_or_else(|| MetricsError::InvalidListenerBind {
                listener: name.to_owned(),
                detail: "Unix socket path is not valid UTF-8",
            }),
    }
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self::new()
    }
}
