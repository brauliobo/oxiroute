#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub struct ListenerMetrics {
    pub(super) process: Arc<ProcessAdmissionState>,
    pub(super) state: Arc<ListenerMetricsState>,
}

impl ListenerMetrics {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.state.name
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.state.protocol
    }

    #[must_use]
    pub fn bind(&self) -> &str {
        &self.state.bind
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        AdministrativeState::from_u8(self.process.administrative_state.load(Ordering::Acquire))
            == AdministrativeState::Ready
            && AdministrativeState::from_u8(
                self.state
                    .shared
                    .administrative_state
                    .load(Ordering::Acquire),
            ) == AdministrativeState::Ready
    }

    /// Marks the listener socket as bound and available to the runtime.
    pub fn mark_listening(&self) {
        self.state
            .runtime_state
            .store(ListenerRuntimeState::Listening as u8, Ordering::Release);
    }

    /// Marks a listener that completed an orderly runtime shutdown.
    pub fn mark_stopped(&self) {
        self.state
            .runtime_state
            .store(ListenerRuntimeState::Stopped as u8, Ordering::Release);
    }

    /// Marks a listener whose runtime terminated unexpectedly.
    pub fn mark_failed(&self) {
        self.state
            .runtime_state
            .store(ListenerRuntimeState::Failed as u8, Ordering::Release);
    }

    /// Accounts for a newly accepted connection.
    ///
    /// The returned guard decrements the active connection count when dropped, including during
    /// unwinding.
    ///
    /// # Errors
    ///
    /// Returns an error if a configured connection cap is reached or the accepted or active
    /// connection counter would overflow.
    #[allow(clippy::too_many_lines)]
    pub fn begin_connection(&self) -> Result<ConnectionGuard, MetricsError> {
        self.begin_connection_for_plane(false)
    }

    #[allow(clippy::too_many_lines)]
    /// Accounts for a control-plane connection while bypassing process administrative drain.
    ///
    /// # Errors
    ///
    /// Returns an error when listener or process capacity is unavailable or accounting overflows.
    pub fn begin_control_connection(&self) -> Result<ConnectionGuard, MetricsError> {
        self.begin_connection_for_plane(true)
    }

    #[allow(clippy::too_many_lines)]
    fn begin_connection_for_plane(&self, control: bool) -> Result<ConnectionGuard, MetricsError> {
        let started_at = Instant::now();
        if AdministrativeState::from_u8(
            self.state
                .shared
                .administrative_state
                .load(Ordering::Acquire),
        ) != AdministrativeState::Ready
        {
            if let Err(error) = checked_atomic_add(
                &self.state.shared.rejected_connections,
                1,
                "listener.rejectedConnections",
            ) {
                self.record_admission_failure(&error, started_at);
                return Err(error);
            }
            let error = MetricsError::AdministrativeDrain {
                resource: "listener",
                name: self.state.name.clone(),
            };
            self.record_admission_failure(&error, started_at);
            return Err(error);
        }
        if let Err(error) = checked_atomic_add(
            &self.state.shared.accepted_connections,
            1,
            "listener.acceptedConnections",
        ) {
            self.record_admission_failure(&error, started_at);
            return Err(error);
        }
        let process = match if control {
            self.process.acquire_control()
        } else {
            self.process.acquire()
        } {
            Ok(process) => process,
            Err(error) => {
                if let Err(accounting_error) = checked_atomic_add(
                    &self.state.shared.rejected_connections,
                    1,
                    "listener.rejectedConnections",
                ) {
                    self.record_admission_failure(&accounting_error, started_at);
                    return Err(accounting_error);
                }
                self.record_admission_failure(&error, started_at);
                return Err(error);
            }
        };
        let admission = self.state.shared.active_connections.try_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| {
                if self
                    .state
                    .shared
                    .limit()
                    .is_some_and(|limit| current >= limit)
                {
                    return None;
                }
                current.checked_add(1)
            },
        );
        if let Err(current) = admission {
            let error =
                if let Some(limit) = self.state.shared.limit().filter(|limit| current >= *limit) {
                    MetricsError::ConnectionLimitReached {
                        listener: self.state.name.clone(),
                        limit,
                    }
                } else {
                    MetricsError::CounterOverflow("listener.activeConnections")
                };
            if let Err(accounting_error) = checked_atomic_add(
                &self.state.shared.rejected_connections,
                1,
                "listener.rejectedConnections",
            ) {
                self.record_admission_failure(&accounting_error, started_at);
                return Err(accounting_error);
            }
            self.record_admission_failure(&error, started_at);
            return Err(error);
        }
        if AdministrativeState::from_u8(
            self.state
                .shared
                .administrative_state
                .load(Ordering::Acquire),
        ) != AdministrativeState::Ready
        {
            decrement_counter(&self.state.shared.active_connections);
            let error = MetricsError::AdministrativeDrain {
                resource: "listener",
                name: self.state.name.clone(),
            };
            if let Err(accounting_error) = checked_atomic_add(
                &self.state.shared.rejected_connections,
                1,
                "listener.rejectedConnections",
            ) {
                self.record_admission_failure(&accounting_error, started_at);
                drop(process);
                return Err(accounting_error);
            }
            self.record_admission_failure(&error, started_at);
            drop(process);
            return Err(error);
        }
        Ok(ConnectionGuard {
            process: Some(process),
            state: Arc::clone(&self.state),
            releases_active_connection: true,
            records_access: true,
            started_at,
            outcome: AtomicU8::new(transport_outcome_index(TransportOutcome::Success)),
            terminal_recorded: AtomicBool::new(false),
            received: AtomicU64::new(0),
            sent: AtomicU64::new(0),
        })
    }

    /// Returns a traffic-only handle for a connection admitted by the server runtime.
    ///
    /// This handle does not acquire or release connection capacity. The runtime-owned admission
    /// guard remains responsible for the active connection lifetime.
    #[must_use]
    pub fn traffic_accounting(&self) -> ConnectionGuard {
        ConnectionGuard {
            process: None,
            state: Arc::clone(&self.state),
            releases_active_connection: false,
            records_access: false,
            started_at: Instant::now(),
            outcome: AtomicU8::new(transport_outcome_index(TransportOutcome::Success)),
            terminal_recorded: AtomicBool::new(false),
            received: AtomicU64::new(0),
            sent: AtomicU64::new(0),
        }
    }

    /// Adds bytes read across this listener to its traffic total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(
            &self.state.shared.bytes_received,
            bytes,
            "listener.bytesReceived",
        )
    }

    /// Adds bytes written across this listener to its traffic total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.shared.bytes_sent, bytes, "listener.bytesSent")
    }

    /// Accounts for a retry that the HTTP proxy handed back to Pingora.
    pub fn record_retry_attempt(&self) {
        let _ = self.process.retry_attempts.try_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| Some(current.saturating_add(1)),
        );
    }

    /// Accounts for a request replayed because the backend answered a rescue status.
    pub fn record_rescue(&self, route: &str, status: u16) {
        let mut rescues = self
            .process
            .rescues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = rescues.entry((route.to_owned(), status)).or_default();
        *count = count.saturating_add(1);
    }

    /// Accounts for a request routed to `pool` by a header selector.
    pub fn record_pool_selection(&self, route: &str, pool: &str) {
        let mut selections = self
            .process
            .pool_selections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = selections
            .entry((route.to_owned(), pool.to_owned()))
            .or_default();
        *count = count.saturating_add(1);
    }

    /// Records one terminal reverse-proxy HTTP operation and its latency sample.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation counter or latency total would overflow.
    pub fn record_http_operation(
        &self,
        result: HttpOperationResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.state.shared.record_http_operation(result, duration)?;
        self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            TransportOutcome::from_http(result),
            Some(duration),
        )
    }

    pub(crate) fn record_cache_event(&self, event: CacheEvent) -> Result<(), MetricsError> {
        self.state.shared.record_cache_event(event)?;
        self.state.shared.transport_operations.record(
            ObservedTransport::Cache,
            cache_event_outcome(event),
            None,
        )
    }

    /// Records one terminal TCP relay and its latency sample.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation counter or latency total would overflow.
    pub fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.state.shared.record_tcp_relay(result, duration)?;
        self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            TransportOutcome::from_tcp(result),
            Some(duration),
        )
    }

    /// Records one redacted PROXY protocol result category.
    ///
    /// # Errors
    ///
    /// Returns an error if the result counter would overflow.
    pub fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        self.state.shared.record_proxy_protocol(result)
    }

    fn record_admission_failure(&self, error: &MetricsError, started_at: Instant) {
        let _ = self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            TransportOutcome::from_admission(error),
            Some(started_at.elapsed()),
        );
    }
}

pub struct ConnectionGuard {
    process: Option<ProcessConnectionGuard>,
    state: Arc<ListenerMetricsState>,
    releases_active_connection: bool,
    records_access: bool,
    started_at: Instant,
    outcome: AtomicU8,
    terminal_recorded: AtomicBool,
    received: AtomicU64,
    sent: AtomicU64,
}

impl ConnectionGuard {
    #[must_use]
    pub fn listener_name(&self) -> &str {
        &self.state.name
    }

    /// Adds bytes read from this connection to its listener total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(
            &self.state.shared.bytes_received,
            bytes,
            "listener.bytesReceived",
        )?;
        checked_atomic_add(&self.received, bytes, "access.bytesReceived")
    }

    /// Adds bytes written to this connection to its listener total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.shared.bytes_sent, bytes, "listener.bytesSent")?;
        checked_atomic_add(&self.sent, bytes, "access.bytesSent")
    }

    pub(crate) fn suppress_access_record(&mut self) {
        self.records_access = false;
    }

    /// Records one terminal TCP relay and its latency sample.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation counter or latency total would overflow.
    pub fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        let outcome = TransportOutcome::from_tcp(result);
        self.state.shared.record_tcp_relay(result, duration)?;
        if let Err(error) = self.state.shared.transport_operations.record(
            transport_for_protocol(&self.state.protocol),
            outcome,
            Some(duration),
        ) {
            self.terminal_recorded.store(true, Ordering::Release);
            self.outcome.store(
                transport_outcome_index(TransportOutcome::InternalError),
                Ordering::Release,
            );
            return Err(error);
        }
        self.terminal_recorded.store(true, Ordering::Release);
        self.outcome
            .store(transport_outcome_index(outcome), Ordering::Release);
        Ok(())
    }

    /// Records one redacted PROXY protocol result category.
    ///
    /// # Errors
    ///
    /// Returns an error if the result counter would overflow.
    pub fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        self.state.shared.record_proxy_protocol(result)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if self.releases_active_connection {
            decrement_counter(&self.state.shared.active_connections);
        }
        if self.records_access {
            let duration = self.started_at.elapsed();
            let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
            let transport = transport_for_protocol(&self.state.protocol);
            let outcome = TransportOutcome::ALL
                .get(self.outcome.load(Ordering::Acquire) as usize)
                .copied()
                .unwrap_or(TransportOutcome::InternalError);
            if transport == ObservedTransport::Udp
                && !self.terminal_recorded.swap(true, Ordering::AcqRel)
            {
                let _ = self.state.shared.transport_operations.record(
                    transport,
                    outcome,
                    Some(duration),
                );
            }
            append_access_record(
                &self.state.shared.access_records,
                AccessRecord {
                    timestamp_unix_ms: unix_time_ms().unwrap_or(0),
                    correlation_id: crate::observability::logging::next_correlation_id(),
                    listener: crate::observability::logging::redact_identifier(&self.state.name),
                    transport,
                    outcome,
                    duration_ms,
                    bytes_received: self.received.load(Ordering::Relaxed),
                    bytes_sent: self.sent.load(Ordering::Relaxed),
                },
            );
        }
        self.process.take();
    }
}

pub(super) struct ProcessAdmissionState {
    pub(super) active: AtomicU64,
    pub(super) administrative_state: AtomicU8,
    limit: AtomicU64,
    pub(super) rejected: AtomicU64,
    pub(super) retry_attempts: AtomicU64,
    rescues: Mutex<BTreeMap<(String, u16), u64>>,
    pool_selections: Mutex<BTreeMap<(String, String), u64>>,
}

impl ProcessAdmissionState {
    pub(super) fn new(limit: Option<u64>) -> Self {
        Self {
            active: AtomicU64::new(0),
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            limit: AtomicU64::new(encode_limit(limit)),
            rejected: AtomicU64::new(0),
            retry_attempts: AtomicU64::new(0),
            rescues: Mutex::new(BTreeMap::new()),
            pool_selections: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn rescue_snapshots(&self) -> Vec<RescueSnapshot> {
        self.rescues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|((route, status), count)| RescueSnapshot {
                route: route.clone(),
                status: *status,
                count: *count,
            })
            .collect()
    }

    pub(super) fn pool_selection_snapshots(&self) -> Vec<PoolSelectionSnapshot> {
        self.pool_selections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|((route, pool), count)| PoolSelectionSnapshot {
                route: route.clone(),
                pool: pool.clone(),
                count: *count,
            })
            .collect()
    }

    pub(super) fn limit(&self) -> Option<u64> {
        decode_limit(self.limit.load(Ordering::Acquire))
    }

    pub(super) fn set_limit(&self, limit: Option<u64>) {
        self.limit.store(encode_limit(limit), Ordering::Release);
    }

    pub(super) fn acquire(self: &Arc<Self>) -> Result<ProcessConnectionGuard, MetricsError> {
        self.acquire_inner(true)
    }

    pub(super) fn acquire_control(
        self: &Arc<Self>,
    ) -> Result<ProcessConnectionGuard, MetricsError> {
        self.acquire_inner(false)
    }

    fn acquire_inner(
        self: &Arc<Self>,
        enforce_administrative_state: bool,
    ) -> Result<ProcessConnectionGuard, MetricsError> {
        if enforce_administrative_state
            && AdministrativeState::from_u8(self.administrative_state.load(Ordering::Acquire))
                != AdministrativeState::Ready
        {
            checked_atomic_add(&self.rejected, 1, "process.rejectedConnections")?;
            return Err(MetricsError::AdministrativeDrain {
                resource: "process",
                name: "oxiroute".into(),
            });
        }
        let admission = self
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if (enforce_administrative_state
                    && AdministrativeState::from_u8(
                        self.administrative_state.load(Ordering::Acquire),
                    ) != AdministrativeState::Ready)
                    || self.limit().is_some_and(|limit| current >= limit)
                {
                    None
                } else {
                    current.checked_add(1)
                }
            });
        match admission {
            Ok(_) => {
                if enforce_administrative_state
                    && AdministrativeState::from_u8(
                        self.administrative_state.load(Ordering::Acquire),
                    ) != AdministrativeState::Ready
                {
                    decrement_counter(&self.active);
                    checked_atomic_add(&self.rejected, 1, "process.rejectedConnections")?;
                    return Err(MetricsError::AdministrativeDrain {
                        resource: "process",
                        name: "oxiroute".into(),
                    });
                }
                Ok(ProcessConnectionGuard {
                    state: Arc::clone(self),
                })
            }
            Err(current) => {
                checked_atomic_add(&self.rejected, 1, "process.rejectedConnections")?;
                Err(
                    if let Some(limit) = self.limit().filter(|limit| current >= *limit) {
                        MetricsError::ProcessConnectionLimitReached { limit }
                    } else {
                        MetricsError::CounterOverflow("process.activeConnections")
                    },
                )
            }
        }
    }
}

pub struct ProcessConnectionGuard {
    state: Arc<ProcessAdmissionState>,
}

impl Drop for ProcessConnectionGuard {
    fn drop(&mut self) {
        decrement_counter(&self.state.active);
    }
}

pub(super) struct ListenerMetricsState {
    pub(super) name: String,
    protocol: String,
    bind: String,
    pub(super) max_connections: Option<u64>,
    pub(super) metric_policy: ListenerMetricPolicy,
    runtime_state: AtomicU8,
    pub(super) shared: Arc<SharedListenerMetricsState>,
}

pub(super) struct SharedListenerMetricsState {
    pub(super) administrative_state: AtomicU8,
    limit: AtomicU64,
    accepted_connections: AtomicU64,
    rejected_connections: AtomicU64,
    active_connections: AtomicU64,
    bytes_received: AtomicU64,
    bytes_sent: AtomicU64,
    operations: OperationMetricsState,
    transport_operations: Arc<TransportOperationsState>,
    access_records: Arc<Mutex<VecDeque<AccessRecord>>>,
}

impl ListenerMetricsState {
    pub(super) fn new(
        name: String,
        protocol: String,
        bind: String,
        max_connections: Option<u64>,
        metric_policy: ListenerMetricPolicy,
        shared: Arc<SharedListenerMetricsState>,
    ) -> Self {
        Self {
            name,
            protocol,
            bind,
            max_connections,
            metric_policy,
            runtime_state: AtomicU8::new(ListenerRuntimeState::Configured as u8),
            shared,
        }
    }

    pub(super) fn snapshot(&self) -> ListenerSnapshot {
        ListenerSnapshot {
            administrative_state: AdministrativeState::from_u8(
                self.shared.administrative_state.load(Ordering::Acquire),
            ),
            name: self.name.clone(),
            protocol: self.protocol.clone(),
            bind: self.bind.clone(),
            max_connections: self.shared.limit(),
            state: ListenerRuntimeState::from_u8(self.runtime_state.load(Ordering::Acquire)),
            accepted_connections: self.shared.accepted_connections.load(Ordering::Relaxed),
            rejected_connections: self.shared.rejected_connections.load(Ordering::Relaxed),
            active_connections: self.shared.active_connections.load(Ordering::Relaxed),
            bytes_received: self.shared.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.shared.bytes_sent.load(Ordering::Relaxed),
            http_operations: self.shared.operations.http_snapshot(),
            tcp_relays: self.shared.operations.tcp_snapshot(),
            proxy_protocol: self.shared.operations.proxy_protocol_snapshot(),
            cache: self.shared.operations.cache_snapshot(),
        }
    }
}

impl SharedListenerMetricsState {
    pub(super) fn new(
        max_connections: Option<u64>,
        transport_operations: Arc<TransportOperationsState>,
        access_records: Arc<Mutex<VecDeque<AccessRecord>>>,
    ) -> Self {
        Self {
            administrative_state: AtomicU8::new(AdministrativeState::Ready as u8),
            limit: AtomicU64::new(encode_limit(max_connections)),
            accepted_connections: AtomicU64::new(0),
            rejected_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            operations: OperationMetricsState::new(),
            transport_operations,
            access_records,
        }
    }

    fn limit(&self) -> Option<u64> {
        decode_limit(self.limit.load(Ordering::Acquire))
    }

    pub(super) fn set_limit(&self, limit: Option<u64>) {
        self.limit.store(encode_limit(limit), Ordering::Release);
    }

    fn record_http_operation(
        &self,
        result: HttpOperationResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.operations.record_http_operation(result, duration)
    }

    fn record_cache_event(&self, event: CacheEvent) -> Result<(), MetricsError> {
        self.operations.record_cache_event(event)
    }

    fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.operations.record_tcp_relay(result, duration)
    }

    fn record_proxy_protocol(&self, result: ProxyProtocolResult) -> Result<(), MetricsError> {
        self.operations.record_proxy_protocol(result)
    }
}

const fn encode_limit(limit: Option<u64>) -> u64 {
    match limit {
        Some(limit) => limit,
        None => 0,
    }
}

const fn decode_limit(limit: u64) -> Option<u64> {
    if limit == 0 { None } else { Some(limit) }
}

pub(super) fn validate_listener_field(
    field: &'static str,
    value: &str,
) -> Result<(), MetricsError> {
    if value.trim().is_empty() {
        return Err(MetricsError::InvalidListenerField(field));
    }
    Ok(())
}
