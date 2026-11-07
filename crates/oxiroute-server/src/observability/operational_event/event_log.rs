#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug)]
pub struct WorkerEventSnapshot {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: String,
    pub outcome: String,
    pub revision: Option<String>,
    pub certificate: Option<String>,
    pub correlation_id: Option<String>,
    pub source: Option<String>,
    pub operation: Option<String>,
}

/// Bounded event page exposed to the supervised worker adapter.
#[derive(Clone, Debug)]
pub struct WorkerEventPage {
    pub events: Vec<WorkerEventSnapshot>,
    pub cursor: u64,
    pub has_more: bool,
    pub oldest_cursor: Option<u64>,
    pub latest_cursor: u64,
    pub cursor_lost: bool,
}

#[derive(Default)]
pub(super) struct EventLog {
    pub(super) next_cursor: u64,
    pub(super) events: VecDeque<OperationalEvent>,
}

pub(super) fn log() -> &'static Mutex<EventLog> {
    static LOG: OnceLock<Mutex<EventLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(EventLog::default()))
}

fn notifications() -> &'static Notify {
    static NOTIFICATIONS: OnceLock<Notify> = OnceLock::new();
    NOTIFICATIONS.get_or_init(Notify::new)
}

fn audit_registry() -> &'static RwLock<Option<Arc<AuditStore>>> {
    static REGISTRY: OnceLock<RwLock<Option<Arc<AuditStore>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(None))
}

pub(crate) fn configure_audit_store(token_file: Option<&Path>) -> Arc<AuditStore> {
    let limits = AuditLimits::from_environment();
    let path = std::env::var_os("OXIROUTE_AUDIT_DIR")
        .map(Into::into)
        .or_else(|| {
            token_file
                .and_then(Path::parent)
                .map(|parent| parent.join("audit"))
        });
    let store = path.map_or_else(
        || AuditStore::memory(limits),
        |path| match AuditStore::open(&path, limits) {
            Ok(store) => store,
            Err(error) => AuditStore::degraded(limits, error.code()),
        },
    );
    let store = Arc::new(store);
    *audit_registry()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&store));
    store
}

fn current_audit_store() -> Arc<AuditStore> {
    if let Some(store) = audit_registry()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .cloned()
    {
        return store;
    }
    configure_audit_store(None)
}

pub(crate) fn audit_status() -> AuditStatus {
    current_audit_store().status()
}

pub(crate) fn audit_metrics() -> AuditMetricSnapshot {
    current_audit_store().metric_snapshot()
}

pub(crate) fn emit(event: &str, outcome: &str, revision: Option<&EffectiveRevision>) {
    emit_with_context(event, outcome, revision, &AuditContext::generated());
}

/// Emits a bounded, redacted RTMP lifecycle event without copying stream queries or credentials.
pub fn emit_rtmp_access(event: &str, outcome: &str) {
    crate::observability::monitoring::record_transport_event(
        ObservedTransport::Rtmp,
        match outcome {
            "accepted" | "closed" => TransportOutcome::Success,
            "rejected" => TransportOutcome::Rejected,
            _ => TransportOutcome::InternalError,
        },
    );
    let event = match event {
        "connect" => "rtmp_connect",
        "publish" => "rtmp_publish",
        "play" => "rtmp_play",
        "disconnect" => "rtmp_disconnect",
        _ => "rtmp_access",
    };
    let outcome = match outcome {
        "accepted" | "closed" => "applied",
        "rejected" => "rejected",
        _ => "failed",
    };
    emit_inner(
        event,
        outcome,
        None,
        None,
        &AuditContext::generated(),
        None,
        None,
    );
}

pub fn emit_certificate(event: &str, outcome: &str, certificate: &str) {
    crate::observability::monitoring::record_transport_event(
        ObservedTransport::Acme,
        match outcome {
            "requested" | "activated" | "applied" => TransportOutcome::Success,
            "failed" => TransportOutcome::UpstreamError,
            "rejected" => TransportOutcome::Rejected,
            _ => TransportOutcome::InternalError,
        },
    );
    emit_certificate_with_context(event, outcome, certificate, &AuditContext::generated());
}

pub(crate) fn emit_with_context(
    event: &str,
    outcome: &str,
    revision: Option<&EffectiveRevision>,
    context: &AuditContext,
) {
    emit_inner(event, outcome, revision, None, context, None, None);
}

pub(crate) fn emit_certificate_with_context(
    event: &str,
    outcome: &str,
    certificate: &str,
    context: &AuditContext,
) {
    emit_inner(event, outcome, None, Some(certificate), context, None, None);
}

/// A retired generation was closed before its work ended; `reason` is `drain_deadline` or `shutdown`.
pub(crate) fn emit_retire_forced_close(revision: &EffectiveRevision, reason: &str) {
    emit_inner(
        "generation_retire",
        "forced_close",
        Some(revision),
        None,
        &AuditContext::generated(),
        Some(reason),
        None,
    );
}

pub(crate) fn emit_api_operation(
    operation: &str,
    category: AuditCategory,
    result: AuditResult,
    revision: Option<&EffectiveRevision>,
    context: &AuditContext,
) {
    let event = match operation {
        "generation_reload" => "generation_prepare",
        "generation_rollback" => "generation_rollback",
        "generation_drain" => "generation_drain",
        "configuration_reload" => "configuration_reload",
        "certificate_reconcile" | "certificate_renew" => "certificate_renewal",
        "certificate_revoke" => "certificate_revocation",
        "certificate_delete" => "certificate_deletion",
        "account_key_rollover" => "certificate_account_rollover",
        "certificate_job_control" => "certificate_job_control",
        "process_shutdown" => "process_shutdown",
        "listener_control" => "listener_administrative_state",
        "pool_control" => "pool_administrative_state",
        "server_control" | "server_dns_refresh" => "server_update",
        _ => "control_operation",
    };
    let outcome = match result {
        AuditResult::Requested => "requested",
        AuditResult::Succeeded | AuditResult::Partial => "applied",
        AuditResult::Failed | AuditResult::Degraded => "failed",
        AuditResult::Rejected | AuditResult::Conflict => "rejected",
    };
    emit_inner(
        event,
        outcome,
        revision,
        None,
        context,
        Some(operation),
        Some((category, result)),
    );
}

/// Hook for the dynamic server registry: a server joined, left or was changed in a pool.
///
/// It is reported as the existing `server_update` event so operators and the audit trail see it
/// without a new event shape; the registry decides when to call it.
pub(crate) fn emit_server_registration(context: &AuditContext) {
    emit_with_context("server_update", "applied", None, context);
}

pub(crate) fn emit_upstream_endpoint_ejection(
    pool: &str,
    server: &str,
    reason: HealthFailure,
    failure_count: u64,
    ejection_count: u64,
    ejected_at_unix_ms: u64,
    ejection_until_unix_ms: u64,
) {
    emit_typed(
        EventName::UpstreamEndpointEjection,
        EventOutcome::Ejected {
            pool: redact_identifier(pool),
            server: redact_identifier(server),
            reason,
            failure_count,
            ejection_count,
            ejected_at_unix_ms,
            ejection_until_unix_ms,
        },
    );
}

pub(crate) fn emit_upstream_endpoint_recovery(
    pool: &str,
    server: &str,
    reason: Option<HealthFailure>,
    recovery_count: u64,
    recovered_at_unix_ms: u64,
) {
    emit_typed(
        EventName::UpstreamEndpointRecovery,
        EventOutcome::Recovered {
            pool: redact_identifier(pool),
            server: redact_identifier(server),
            reason,
            recovery_count,
            recovered_at_unix_ms,
        },
    );
}

pub(crate) fn emit_upstream_endpoint_health(
    pool: &str,
    server: &str,
    previous: EndpointHealthState,
    next: EndpointHealthState,
    reason: Option<HealthFailure>,
    changed_at_unix_ms: u64,
) {
    emit_typed(
        EventName::UpstreamEndpointHealth,
        EventOutcome::HealthChanged {
            pool: redact_identifier(pool),
            server: redact_identifier(server),
            previous,
            next,
            reason,
            changed_at_unix_ms,
        },
    );
}

fn emit_typed(event: EventName, outcome: EventOutcome) {
    let context = AuditContext::generated();
    let category = category_for_event(event);
    let result = result_for_outcome(&outcome);
    let mut state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut value = OperationalEvent {
        cursor: 0,
        timestamp_unix_ms: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        event,
        outcome,
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let audit_value = publish(&mut state, &mut value, &context, None);
    drop(state);
    persist(audit_value, &context, category, result);
}

fn emit_inner(
    event: &str,
    outcome: &str,
    revision: Option<&EffectiveRevision>,
    certificate: Option<&str>,
    context: &AuditContext,
    operation: Option<&str>,
    category_result: Option<(AuditCategory, AuditResult)>,
) {
    let mut state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut value = OperationalEvent::new(
        0,
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        event,
        outcome,
        revision,
    );
    value.certificate = certificate.map(redact_identifier);
    let event_category =
        category_result.map_or_else(|| category_for_event(value.event), |(category, _)| category);
    let event_result =
        category_result.map_or_else(|| result_for_outcome(&value.outcome), |(_, result)| result);
    let audit_value = publish(&mut state, &mut value, context, operation);
    drop(state);
    persist(audit_value, context, event_category, event_result);
}

fn publish(
    state: &mut EventLog,
    value: &mut OperationalEvent,
    context: &AuditContext,
    operation: Option<&str>,
) -> OperationalEvent {
    state.next_cursor = state.next_cursor.saturating_add(1);
    value.cursor = state.next_cursor;
    value.correlation_id = Some(redact_identifier(&context.correlation_id));
    value.actor = Some(redact_identifier(&context.actor));
    value.source = Some(redact_identifier(&context.source));
    value.operation = Some(redact_identifier(operation.unwrap_or(value.event.as_str())));
    if state.events.len() == EVENT_CAPACITY {
        state.events.pop_front();
    }
    state.events.push_back(value.clone());
    value.clone()
}

#[allow(clippy::needless_pass_by_value)]
fn persist(
    audit_value: OperationalEvent,
    context: &AuditContext,
    category: AuditCategory,
    result: AuditResult,
) {
    let revision = audit_value.revision.as_ref();
    if let Err(error) = current_audit_store().append(
        context,
        category,
        audit_value.operation.as_deref().unwrap_or("unknown"),
        result,
        revision,
    ) {
        log::warn!(target: "oxiroute::audit", "durable audit write failed: {}", error.code());
    }
    if let Ok(json) = serde_json::to_string(&audit_value) {
        log::info!(target: "oxiroute::operations", "{json}");
    }
    notifications().notify_one();
}

pub(super) fn unix_time_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

pub(super) const fn category_for_event(event: EventName) -> AuditCategory {
    match event {
        EventName::GenerationPrepare
        | EventName::GenerationActivate
        | EventName::GenerationRollback
        | EventName::GenerationDrain
        | EventName::GenerationRetire
        | EventName::GenerationStart
        | EventName::ConfigurationReload => AuditCategory::Reload,
        EventName::ImportCompleted => AuditCategory::Import,
        EventName::CertificateRenewal
        | EventName::CertificateActivation
        | EventName::CertificateRevocation
        | EventName::CertificateDeletion
        | EventName::CertificateAccountRollover
        | EventName::CertificateJobControl => AuditCategory::Certificate,
        EventName::ControlOperation
        | EventName::ProcessShutdown
        | EventName::ListenerAdministrativeState
        | EventName::PoolAdministrativeState
        | EventName::ServerUpdate
        | EventName::RtmpConnect
        | EventName::RtmpPublish
        | EventName::RtmpPlay
        | EventName::RtmpDisconnect
        | EventName::RtmpAccess
        | EventName::UpstreamEndpointEjection
        | EventName::UpstreamEndpointRecovery
        | EventName::UpstreamEndpointHealth
        | EventName::Unknown => AuditCategory::Control,
    }
}

const fn result_for_outcome(outcome: &EventOutcome) -> AuditResult {
    match outcome {
        EventOutcome::Prepared
        | EventOutcome::Activated
        | EventOutcome::Applied
        | EventOutcome::Retiring
        | EventOutcome::Drained
        | EventOutcome::Ejected { .. }
        | EventOutcome::Recovered { .. }
        | EventOutcome::HealthChanged { .. } => AuditResult::Succeeded,
        EventOutcome::Requested | EventOutcome::CapReached => AuditResult::Requested,
        EventOutcome::Rejected => AuditResult::Rejected,
        EventOutcome::Failed
        | EventOutcome::ForcedClose
        | EventOutcome::Quarantined
        | EventOutcome::Unknown => AuditResult::Failed,
    }
}

pub(crate) fn page(after: u64, limit: usize) -> EventPage {
    let state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.page(after, limit)
}

/// Returns a bounded, redacted event page without exposing internal event enums.
#[must_use]
pub fn worker_event_page(after: u64, limit: usize) -> WorkerEventPage {
    let EventPage {
        events,
        cursor,
        has_more,
        oldest_cursor,
        latest_cursor,
        cursor_lost,
    } = page(after, limit);
    WorkerEventPage {
        events: events
            .into_iter()
            .map(|event| WorkerEventSnapshot {
                cursor: event.cursor,
                timestamp_unix_ms: event.timestamp_unix_ms,
                event: event.event.as_str().to_owned(),
                outcome: event.outcome.as_str().to_owned(),
                revision: event.revision.map(|revision| revision.to_string()),
                certificate: event.certificate,
                correlation_id: event.correlation_id,
                source: event.source,
                operation: event.operation,
            })
            .collect(),
        cursor,
        has_more,
        oldest_cursor,
        latest_cursor,
        cursor_lost,
    }
}

pub(crate) fn current_cursor() -> u64 {
    log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .next_cursor
}

pub(crate) async fn wait_for_event() {
    notifications().notified().await;
}

impl EventLog {
    pub(super) fn page(&self, after: u64, limit: usize) -> EventPage {
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.cursor > after)
            .take(limit.min(EVENT_CAPACITY))
            .cloned()
            .collect();
        let cursor = events.last().map_or(after, |event| event.cursor);
        let has_more = self.events.iter().any(|event| event.cursor > cursor);
        let oldest_cursor = self.events.front().map(|event| event.cursor);
        EventPage {
            events,
            cursor,
            has_more,
            oldest_cursor,
            latest_cursor: self.next_cursor,
            cursor_lost: oldest_cursor.is_some_and(|oldest| after < oldest.saturating_sub(1)),
        }
    }
}
