use std::collections::{BTreeMap, VecDeque};

use oxiroute_supervision::{
    BoundedWireProtocol, BoundedWireReader, BoundedWireWriter, GenerationId, GenerationRole,
    InstanceId, SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::MessageType;
use oxiroute_supervisor_process::AuthenticatedFrame;
use thiserror::Error;

use crate::CONTROL_PROTOCOL_VERSION;

pub(crate) const STATUS_MESSAGE: MessageType = MessageType(0x181);
/// Maximum encoded worker status bytes accepted by either endpoint.
pub const MAX_STATUS_BYTES: usize = 60 * 1024;
/// Maximum listener observations carried by one worker status.
pub const MAX_STATUS_LISTENERS: usize = 64;
/// Maximum event observations carried by one worker status.
pub const MAX_STATUS_EVENTS: usize = 64;
/// Maximum worker-originated events retained by the master.
pub const MAX_AGGREGATED_EVENTS: usize = 2_048;

const MAX_STATUS_STRING_BYTES: usize = 1_024;
const STATUS_FORMAT_VERSION: u16 = 1;

/// Lifecycle observation reported by a worker. It is not a command surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerLifecycle {
    Starting,
    Ready,
    Active,
    Quiescing,
    Draining,
    Reactivating,
    Stopping,
    Failed,
}

/// Runtime listener state reported by a worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerListenerState {
    Configured,
    Listening,
    Stopped,
    Failed,
}

/// Administrative listener state reported by a worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerAdministrativeState {
    Ready,
    Drain,
    Maintenance,
}

/// Aggregate process and traffic counters reported by a worker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerMetrics {
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// One bounded listener observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerListenerStatus {
    pub name: String,
    pub protocol: String,
    pub bind: String,
    pub administrative_state: WorkerAdministrativeState,
    pub state: WorkerListenerState,
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// Generation/reload state observed inside a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerGenerationStatus {
    pub disk_revision: Option<String>,
    pub candidate_revision: Option<String>,
    pub active_revision: Option<String>,
    pub previous_revision: Option<String>,
    pub quarantined_revision: Option<String>,
    pub active_accepting: bool,
    pub degraded: bool,
    pub last_failure: Option<String>,
    pub prepares: u64,
    pub activations: u64,
    pub failures: u64,
    pub rollbacks: u64,
}

/// One redacted worker-originated operational event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerEventRecord {
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

/// Authenticated, bounded worker observation sent to the master.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct WorkerStatus {
    pub sequence: u64,
    pub generation_id: GenerationId,
    pub lifecycle: WorkerLifecycle,
    pub administrative_state: WorkerAdministrativeState,
    pub accepting: bool,
    pub runtime_started: bool,
    pub runtime_failed: bool,
    pub drained: bool,
    pub generation: WorkerGenerationStatus,
    pub metrics: Option<WorkerMetrics>,
    pub listeners: Vec<WorkerListenerStatus>,
    pub degraded: bool,
    pub degradation: Option<String>,
    pub event_cursor: u64,
    pub event_cursor_lost: bool,
    pub events: Vec<WorkerEventRecord>,
}

/// A worker event retained by the master with authenticated process identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregatedWorkerEvent {
    pub cursor: u64,
    pub instance_id: InstanceId,
    pub generation_id: GenerationId,
    pub worker_event: WorkerEventRecord,
}

/// One catalog-owned generation projected without its launch payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorGenerationSnapshot<R> {
    pub role: GenerationRole,
    pub instance_id: InstanceId,
    pub generation_id: GenerationId,
    pub revision: R,
}

/// One current process observation matched to its catalog identity.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct SupervisorProcessSnapshot {
    pub role: GenerationRole,
    pub instance_id: InstanceId,
    pub generation_id: GenerationId,
    pub sequence: u64,
    pub lifecycle: WorkerLifecycle,
    pub administrative_state: WorkerAdministrativeState,
    pub accepting: bool,
    pub runtime_started: bool,
    pub runtime_failed: bool,
    pub drained: bool,
    pub generation: WorkerGenerationStatus,
}

/// A role-qualified listener observation from one current process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorListenerObservation {
    pub role: GenerationRole,
    pub instance_id: InstanceId,
    pub listener: WorkerListenerStatus,
}

/// Monotonic counters for one logical listener across live and reaped workers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorListenerSnapshot {
    pub name: String,
    pub protocol: String,
    pub bind: String,
    pub administrative_state: WorkerAdministrativeState,
    pub state: WorkerListenerState,
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// A role-qualified degradation observation from one current process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorDegradation {
    pub role: GenerationRole,
    pub instance_id: InstanceId,
    pub reason: Option<String>,
}

/// A bounded worker event with the role occupied when the master observed it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorEventRecord {
    pub role: GenerationRole,
    pub event: AggregatedWorkerEvent,
}

/// Read-only master projection of catalog state and matched process observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorSnapshot<R> {
    pub generations: Vec<SupervisorGenerationSnapshot<R>>,
    pub processes: Vec<SupervisorProcessSnapshot>,
    pub listener_observations: Vec<SupervisorListenerObservation>,
    pub listeners: Vec<SupervisorListenerSnapshot>,
    pub degraded: bool,
    pub degradation: Vec<SupervisorDegradation>,
    pub metrics: WorkerMetrics,
    pub events: Vec<SupervisorEventRecord>,
}

/// A catalog/process identity mismatch while building a supervisor snapshot.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error(
    "process observation for {instance_id} in {observed_role:?} does not match catalog role {catalog_role:?}"
)]
pub struct SupervisorSnapshotError {
    pub instance_id: InstanceId,
    pub observed_role: GenerationRole,
    pub catalog_role: Option<GenerationRole>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerSnapshotSource<'a> {
    pub(crate) role: GenerationRole,
    pub(crate) instance_id: &'a InstanceId,
    pub(crate) generation_id: GenerationId,
    pub(crate) status: &'a WorkerStatus,
}

#[derive(Debug)]
struct RetainedEvent {
    role: GenerationRole,
    event: AggregatedWorkerEvent,
}

#[derive(Debug, Default)]
pub(crate) struct SupervisorSnapshotHistory {
    metrics: WorkerMetrics,
    listeners: BTreeMap<(String, String, String), SupervisorListenerSnapshot>,
    events: VecDeque<RetainedEvent>,
    next_event_cursor: u64,
}

impl SupervisorSnapshotHistory {
    pub(crate) fn new() -> Self {
        Self {
            next_event_cursor: 1,
            ..Self::default()
        }
    }

    pub(crate) fn record_events(
        &mut self,
        role: GenerationRole,
        instance_id: &InstanceId,
        generation_id: GenerationId,
        previous_cursor: u64,
        status: &WorkerStatus,
    ) -> u64 {
        let mut event_cursor = previous_cursor;
        for worker_event in &status.events {
            if worker_event.cursor <= event_cursor {
                continue;
            }
            let cursor = self.next_event_cursor;
            if cursor == 0 {
                break;
            }
            self.next_event_cursor = cursor.checked_add(1).unwrap_or(0);
            self.events.push_back(RetainedEvent {
                role,
                event: AggregatedWorkerEvent {
                    cursor,
                    instance_id: instance_id.clone(),
                    generation_id,
                    worker_event: worker_event.clone(),
                },
            });
            while self.events.len() > MAX_AGGREGATED_EVENTS {
                self.events.pop_front();
            }
            event_cursor = worker_event.cursor;
        }
        event_cursor.max(status.event_cursor)
    }

    pub(crate) fn reap(&mut self, status: Option<&WorkerStatus>) {
        let Some(status) = status else {
            return;
        };
        if let Some(metrics) = status.metrics {
            add_cumulative_metrics(&mut self.metrics, metrics);
        }
        for listener in &status.listeners {
            let entry = self
                .listeners
                .entry(listener_key(listener))
                .or_insert_with(|| listener_snapshot(listener));
            add_cumulative_listener(entry, listener);
            entry.active_connections = 0;
        }
    }

    pub(crate) fn worker_events(&self, after: u64, limit: usize) -> Vec<AggregatedWorkerEvent> {
        self.events
            .iter()
            .filter(|retained| retained.event.cursor > after)
            .take(limit.min(MAX_AGGREGATED_EVENTS))
            .map(|retained| retained.event.clone())
            .collect()
    }

    pub(crate) fn snapshot<R: Clone, T>(
        &self,
        catalog: &SupervisedGenerationCatalog<R, T>,
        sources: &[WorkerSnapshotSource<'_>],
    ) -> Result<SupervisorSnapshot<R>, SupervisorSnapshotError> {
        let generations = catalog
            .documents()
            .map(|(role, document)| SupervisorGenerationSnapshot {
                role,
                instance_id: document.instance_id().clone(),
                generation_id: document.generation_id(),
                revision: document.revision().clone(),
            })
            .collect::<Vec<_>>();
        for source in sources {
            let catalog_role = catalog.documents().find_map(|(role, document)| {
                (document.instance_id() == source.instance_id
                    && document.generation_id() == source.generation_id)
                    .then_some(role)
            });
            if catalog_role != Some(source.role) {
                return Err(SupervisorSnapshotError {
                    instance_id: source.instance_id.clone(),
                    observed_role: source.role,
                    catalog_role,
                });
            }
        }

        let mut metrics = self.metrics;
        let mut listeners = self.listeners.clone();
        let mut listener_priorities = BTreeMap::new();
        let mut processes = Vec::with_capacity(sources.len());
        let mut listener_observations = Vec::new();
        let mut degradation = Vec::new();
        for source in sources {
            let status = source.status;
            if let Some(worker_metrics) = status.metrics {
                add_all_metrics(&mut metrics, worker_metrics);
            }
            processes.push(SupervisorProcessSnapshot {
                role: source.role,
                instance_id: source.instance_id.clone(),
                generation_id: source.generation_id,
                sequence: status.sequence,
                lifecycle: status.lifecycle,
                administrative_state: status.administrative_state,
                accepting: status.accepting,
                runtime_started: status.runtime_started,
                runtime_failed: status.runtime_failed,
                drained: status.drained,
                generation: status.generation.clone(),
            });
            for listener in &status.listeners {
                listener_observations.push(SupervisorListenerObservation {
                    role: source.role,
                    instance_id: source.instance_id.clone(),
                    listener: listener.clone(),
                });
                let key = listener_key(listener);
                let priority = role_priority(source.role);
                let entry = listeners
                    .entry(key.clone())
                    .or_insert_with(|| listener_snapshot(listener));
                add_all_listener(entry, listener);
                if listener_priorities
                    .get(&key)
                    .is_none_or(|current| priority > *current)
                {
                    entry.administrative_state = listener.administrative_state;
                    entry.state = listener.state;
                    listener_priorities.insert(key, priority);
                }
            }
            if status.degraded || status.generation.degraded || status.runtime_failed {
                degradation.push(SupervisorDegradation {
                    role: source.role,
                    instance_id: source.instance_id.clone(),
                    reason: status
                        .degradation
                        .clone()
                        .or_else(|| status.generation.last_failure.clone()),
                });
            }
        }
        let events = self
            .events
            .iter()
            .map(|retained| SupervisorEventRecord {
                role: retained.role,
                event: retained.event.clone(),
            })
            .collect();
        Ok(SupervisorSnapshot {
            generations,
            processes,
            listener_observations,
            listeners: listeners.into_values().collect(),
            degraded: !degradation.is_empty(),
            degradation,
            metrics,
            events,
        })
    }
}

pub(crate) fn retain_monotonic_status(previous: &WorkerStatus, status: &mut WorkerStatus) {
    match (&previous.metrics, &mut status.metrics) {
        (Some(previous), Some(current)) => retain_monotonic_metrics(*previous, current),
        (Some(previous), None) => status.metrics = Some(*previous),
        (None, _) => {}
    }
    status.generation.prepares = status.generation.prepares.max(previous.generation.prepares);
    status.generation.activations = status
        .generation
        .activations
        .max(previous.generation.activations);
    status.generation.failures = status.generation.failures.max(previous.generation.failures);
    status.generation.rollbacks = status
        .generation
        .rollbacks
        .max(previous.generation.rollbacks);
    for previous_listener in &previous.listeners {
        if let Some(current) = status.listeners.iter_mut().find(|current| {
            current.name == previous_listener.name
                && current.protocol == previous_listener.protocol
                && current.bind == previous_listener.bind
        }) {
            retain_monotonic_listener(previous_listener, current);
        } else {
            status.listeners.push(previous_listener.clone());
        }
    }
}

fn listener_key(listener: &WorkerListenerStatus) -> (String, String, String) {
    (
        listener.name.clone(),
        listener.protocol.clone(),
        listener.bind.clone(),
    )
}

fn listener_snapshot(listener: &WorkerListenerStatus) -> SupervisorListenerSnapshot {
    SupervisorListenerSnapshot {
        name: listener.name.clone(),
        protocol: listener.protocol.clone(),
        bind: listener.bind.clone(),
        administrative_state: listener.administrative_state,
        state: listener.state,
        accepted_connections: 0,
        rejected_connections: 0,
        active_connections: 0,
        bytes_received: 0,
        bytes_sent: 0,
    }
}

fn add_cumulative_metrics(total: &mut WorkerMetrics, value: WorkerMetrics) {
    total.accepted_connections = total
        .accepted_connections
        .saturating_add(value.accepted_connections);
    total.rejected_connections = total
        .rejected_connections
        .saturating_add(value.rejected_connections);
    total.bytes_received = total.bytes_received.saturating_add(value.bytes_received);
    total.bytes_sent = total.bytes_sent.saturating_add(value.bytes_sent);
}

fn add_all_metrics(total: &mut WorkerMetrics, value: WorkerMetrics) {
    add_cumulative_metrics(total, value);
    total.active_connections = total
        .active_connections
        .saturating_add(value.active_connections);
}

fn retain_monotonic_metrics(previous: WorkerMetrics, current: &mut WorkerMetrics) {
    current.accepted_connections = current
        .accepted_connections
        .max(previous.accepted_connections);
    current.rejected_connections = current
        .rejected_connections
        .max(previous.rejected_connections);
    current.bytes_received = current.bytes_received.max(previous.bytes_received);
    current.bytes_sent = current.bytes_sent.max(previous.bytes_sent);
}

fn add_cumulative_listener(total: &mut SupervisorListenerSnapshot, value: &WorkerListenerStatus) {
    total.accepted_connections = total
        .accepted_connections
        .saturating_add(value.accepted_connections);
    total.rejected_connections = total
        .rejected_connections
        .saturating_add(value.rejected_connections);
    total.bytes_received = total.bytes_received.saturating_add(value.bytes_received);
    total.bytes_sent = total.bytes_sent.saturating_add(value.bytes_sent);
}

fn add_all_listener(total: &mut SupervisorListenerSnapshot, value: &WorkerListenerStatus) {
    add_cumulative_listener(total, value);
    total.active_connections = total
        .active_connections
        .saturating_add(value.active_connections);
}

fn retain_monotonic_listener(previous: &WorkerListenerStatus, current: &mut WorkerListenerStatus) {
    current.accepted_connections = current
        .accepted_connections
        .max(previous.accepted_connections);
    current.rejected_connections = current
        .rejected_connections
        .max(previous.rejected_connections);
    current.bytes_received = current.bytes_received.max(previous.bytes_received);
    current.bytes_sent = current.bytes_sent.max(previous.bytes_sent);
}

const fn role_priority(role: GenerationRole) -> u8 {
    match role {
        GenerationRole::Active => 5,
        GenerationRole::Candidate => 4,
        GenerationRole::Previous => 3,
        GenerationRole::Quarantined => 2,
        GenerationRole::RestartRequired => 1,
    }
}

#[derive(Debug, Error)]
pub enum StatusProtocolError {
    #[error("unexpected worker status message type {0}")]
    UnexpectedMessage(u16),
    #[error("worker status transferred unexpected descriptors")]
    UnexpectedDescriptors,
    #[error("worker status payload exceeds {maximum} bytes")]
    TooLarge { maximum: usize },
    #[error("worker status protocol version {actual} does not match {expected}")]
    VersionMismatch { expected: u16, actual: u16 },
    #[error("worker status payload has an invalid shape")]
    InvalidPayload,
    #[error("worker status contains a string longer than {maximum} bytes")]
    StringTooLong { maximum: usize },
    #[error("worker status contains too many {kind}")]
    CollectionTooLong { kind: &'static str },
    #[error("worker status allocation failed")]
    Allocation,
}

pub(crate) fn encode_status(status: &WorkerStatus) -> Result<Vec<u8>, StatusProtocolError> {
    if status.sequence == 0 || status.listeners.len() > MAX_STATUS_LISTENERS {
        return Err(StatusProtocolError::InvalidPayload);
    }
    if status.events.len() > MAX_STATUS_EVENTS {
        return Err(StatusProtocolError::CollectionTooLong { kind: "events" });
    }
    let mut encoder = Encoder::new(MAX_STATUS_BYTES);
    encoder.u16(STATUS_FORMAT_VERSION)?;
    encoder.u16(CONTROL_PROTOCOL_VERSION)?;
    encoder.u64(status.sequence)?;
    encoder.u64(status.generation_id.0)?;
    encoder.u8(encode_lifecycle(status.lifecycle))?;
    encoder.u8(status_flags(status))?;
    encoder.u8(encode_administrative_state(status.administrative_state))?;
    encode_generation(&mut encoder, &status.generation)?;
    match status.metrics {
        Some(metrics) => {
            encoder.u8(1)?;
            encoder.u64(metrics.accepted_connections)?;
            encoder.u64(metrics.rejected_connections)?;
            encoder.u64(metrics.active_connections)?;
            encoder.u64(metrics.bytes_received)?;
            encoder.u64(metrics.bytes_sent)?;
        }
        None => encoder.u8(0)?,
    }
    encoder.u16(
        u16::try_from(status.listeners.len()).map_err(|_| StatusProtocolError::InvalidPayload)?,
    )?;
    for listener in &status.listeners {
        encoder.string(&listener.name)?;
        encoder.string(&listener.protocol)?;
        encoder.string(&listener.bind)?;
        encoder.u8(encode_administrative_state(listener.administrative_state))?;
        encoder.u8(encode_listener_state(listener.state))?;
        encoder.u64(listener.accepted_connections)?;
        encoder.u64(listener.rejected_connections)?;
        encoder.u64(listener.active_connections)?;
        encoder.u64(listener.bytes_received)?;
        encoder.u64(listener.bytes_sent)?;
    }
    encoder.u8(u8::from(status.degradation.is_some()))?;
    if let Some(degradation) = &status.degradation {
        encoder.string(degradation)?;
    }
    encoder.u64(status.event_cursor)?;
    encoder.u8(u8::from(status.event_cursor_lost))?;
    encoder.u16(
        u16::try_from(status.events.len()).map_err(|_| StatusProtocolError::InvalidPayload)?,
    )?;
    for event in &status.events {
        encode_event(&mut encoder, event)?;
    }
    Ok(encoder.finish())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn decode_status(
    frame: &AuthenticatedFrame,
) -> Result<WorkerStatus, StatusProtocolError> {
    if frame.header().message_type() != STATUS_MESSAGE {
        return Err(StatusProtocolError::UnexpectedMessage(
            frame.header().message_type().0,
        ));
    }
    if !frame.descriptors().is_empty() {
        return Err(StatusProtocolError::UnexpectedDescriptors);
    }
    if frame.payload().len() > MAX_STATUS_BYTES {
        return Err(StatusProtocolError::TooLarge {
            maximum: MAX_STATUS_BYTES,
        });
    }
    let mut decoder = Decoder::new(frame.payload());
    let format_version = decoder.u16()?;
    if format_version != STATUS_FORMAT_VERSION {
        return Err(StatusProtocolError::VersionMismatch {
            expected: STATUS_FORMAT_VERSION,
            actual: format_version,
        });
    }
    let protocol_version = decoder.u16()?;
    if protocol_version != CONTROL_PROTOCOL_VERSION {
        return Err(StatusProtocolError::VersionMismatch {
            expected: CONTROL_PROTOCOL_VERSION,
            actual: protocol_version,
        });
    }
    let sequence = decoder.u64()?;
    if sequence == 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let generation_id = GenerationId(decoder.u64()?);
    if generation_id != frame.header().generation() {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let lifecycle = decode_lifecycle(decoder.u8()?)?;
    let flags = decoder.u8()?;
    if flags & !0x1f != 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let administrative_state = decode_administrative_state(decoder.u8()?)?;
    let generation = decode_generation(&mut decoder)?;
    let metrics = match decoder.u8()? {
        0 => None,
        1 => Some(WorkerMetrics {
            accepted_connections: decoder.u64()?,
            rejected_connections: decoder.u64()?,
            active_connections: decoder.u64()?,
            bytes_received: decoder.u64()?,
            bytes_sent: decoder.u64()?,
        }),
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    let listener_count = usize::from(decoder.u16()?);
    if listener_count > MAX_STATUS_LISTENERS {
        return Err(StatusProtocolError::CollectionTooLong { kind: "listeners" });
    }
    let mut listeners = Vec::new();
    listeners
        .try_reserve_exact(listener_count)
        .map_err(|_| StatusProtocolError::Allocation)?;
    for _ in 0..listener_count {
        listeners.push(WorkerListenerStatus {
            name: decoder.string()?,
            protocol: decoder.string()?,
            bind: decoder.string()?,
            administrative_state: decode_administrative_state(decoder.u8()?)?,
            state: decode_listener_state(decoder.u8()?)?,
            accepted_connections: decoder.u64()?,
            rejected_connections: decoder.u64()?,
            active_connections: decoder.u64()?,
            bytes_received: decoder.u64()?,
            bytes_sent: decoder.u64()?,
        });
    }
    let degradation = match decoder.u8()? {
        0 => None,
        1 => Some(decoder.string()?),
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    let event_cursor = decoder.u64()?;
    let event_cursor_lost = match decoder.u8()? {
        0 => false,
        1 => true,
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    let event_count = usize::from(decoder.u16()?);
    if event_count > MAX_STATUS_EVENTS {
        return Err(StatusProtocolError::CollectionTooLong { kind: "events" });
    }
    let mut events = Vec::new();
    events
        .try_reserve_exact(event_count)
        .map_err(|_| StatusProtocolError::Allocation)?;
    for _ in 0..event_count {
        events.push(decode_event(&mut decoder)?);
    }
    if events
        .windows(2)
        .any(|events| events[0].cursor >= events[1].cursor)
    {
        return Err(StatusProtocolError::InvalidPayload);
    }
    if events
        .last()
        .is_some_and(|event| event.cursor > event_cursor)
    {
        return Err(StatusProtocolError::InvalidPayload);
    }
    decoder.finish()?;
    Ok(WorkerStatus {
        sequence,
        generation_id,
        lifecycle,
        administrative_state,
        accepting: flags & 0x01 != 0,
        runtime_started: flags & 0x02 != 0,
        runtime_failed: flags & 0x04 != 0,
        drained: flags & 0x08 != 0,
        degraded: flags & 0x10 != 0,
        generation,
        metrics,
        listeners,
        degradation,
        event_cursor,
        event_cursor_lost,
        events,
    })
}

fn encode_generation(
    encoder: &mut Encoder,
    generation: &WorkerGenerationStatus,
) -> Result<(), StatusProtocolError> {
    for revision in [
        &generation.disk_revision,
        &generation.candidate_revision,
        &generation.active_revision,
        &generation.previous_revision,
        &generation.quarantined_revision,
    ] {
        encoder.optional_string(revision.as_deref())?;
    }
    encoder.u8(u8::from(generation.active_accepting))?;
    encoder.u8(u8::from(generation.degraded))?;
    encoder.optional_string(generation.last_failure.as_deref())?;
    encoder.u64(generation.prepares)?;
    encoder.u64(generation.activations)?;
    encoder.u64(generation.failures)?;
    encoder.u64(generation.rollbacks)
}

fn decode_generation(
    decoder: &mut Decoder<'_>,
) -> Result<WorkerGenerationStatus, StatusProtocolError> {
    Ok(WorkerGenerationStatus {
        disk_revision: decoder.optional_string()?,
        candidate_revision: decoder.optional_string()?,
        active_revision: decoder.optional_string()?,
        previous_revision: decoder.optional_string()?,
        quarantined_revision: decoder.optional_string()?,
        active_accepting: decoder.flag()?,
        degraded: decoder.flag()?,
        last_failure: decoder.optional_string()?,
        prepares: decoder.u64()?,
        activations: decoder.u64()?,
        failures: decoder.u64()?,
        rollbacks: decoder.u64()?,
    })
}

fn encode_event(
    encoder: &mut Encoder,
    event: &WorkerEventRecord,
) -> Result<(), StatusProtocolError> {
    if event.cursor == 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    encoder.u64(event.cursor)?;
    match event.timestamp_unix_ms {
        Some(timestamp) => {
            encoder.u8(1)?;
            encoder.u64(timestamp)?;
        }
        None => encoder.u8(0)?,
    }
    encoder.string(&event.event)?;
    encoder.string(&event.outcome)?;
    encoder.optional_string(event.revision.as_deref())?;
    encoder.optional_string(event.certificate.as_deref())?;
    encoder.optional_string(event.correlation_id.as_deref())?;
    encoder.optional_string(event.source.as_deref())?;
    encoder.optional_string(event.operation.as_deref())
}

fn decode_event(decoder: &mut Decoder<'_>) -> Result<WorkerEventRecord, StatusProtocolError> {
    let cursor = decoder.u64()?;
    if cursor == 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let timestamp_unix_ms = match decoder.u8()? {
        0 => None,
        1 => Some(decoder.u64()?),
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    Ok(WorkerEventRecord {
        cursor,
        timestamp_unix_ms,
        event: decoder.string()?,
        outcome: decoder.string()?,
        revision: decoder.optional_string()?,
        certificate: decoder.optional_string()?,
        correlation_id: decoder.optional_string()?,
        source: decoder.optional_string()?,
        operation: decoder.optional_string()?,
    })
}

const fn status_flags(status: &WorkerStatus) -> u8 {
    (if status.accepting { 1 } else { 0 })
        | ((if status.runtime_started { 1 } else { 0 }) << 1)
        | ((if status.runtime_failed { 1 } else { 0 }) << 2)
        | ((if status.drained { 1 } else { 0 }) << 3)
        | ((if status.degraded { 1 } else { 0 }) << 4)
}

const fn encode_lifecycle(lifecycle: WorkerLifecycle) -> u8 {
    match lifecycle {
        WorkerLifecycle::Starting => 1,
        WorkerLifecycle::Ready => 2,
        WorkerLifecycle::Active => 3,
        WorkerLifecycle::Quiescing => 4,
        WorkerLifecycle::Draining => 5,
        WorkerLifecycle::Reactivating => 6,
        WorkerLifecycle::Stopping => 7,
        WorkerLifecycle::Failed => 8,
    }
}

const fn decode_lifecycle(value: u8) -> Result<WorkerLifecycle, StatusProtocolError> {
    match value {
        1 => Ok(WorkerLifecycle::Starting),
        2 => Ok(WorkerLifecycle::Ready),
        3 => Ok(WorkerLifecycle::Active),
        4 => Ok(WorkerLifecycle::Quiescing),
        5 => Ok(WorkerLifecycle::Draining),
        6 => Ok(WorkerLifecycle::Reactivating),
        7 => Ok(WorkerLifecycle::Stopping),
        8 => Ok(WorkerLifecycle::Failed),
        _ => Err(StatusProtocolError::InvalidPayload),
    }
}

const fn encode_listener_state(state: WorkerListenerState) -> u8 {
    match state {
        WorkerListenerState::Configured => 0,
        WorkerListenerState::Listening => 1,
        WorkerListenerState::Stopped => 2,
        WorkerListenerState::Failed => 3,
    }
}

const fn decode_listener_state(value: u8) -> Result<WorkerListenerState, StatusProtocolError> {
    match value {
        0 => Ok(WorkerListenerState::Configured),
        1 => Ok(WorkerListenerState::Listening),
        2 => Ok(WorkerListenerState::Stopped),
        3 => Ok(WorkerListenerState::Failed),
        _ => Err(StatusProtocolError::InvalidPayload),
    }
}

const fn encode_administrative_state(state: WorkerAdministrativeState) -> u8 {
    match state {
        WorkerAdministrativeState::Ready => 0,
        WorkerAdministrativeState::Drain => 1,
        WorkerAdministrativeState::Maintenance => 2,
    }
}

const fn decode_administrative_state(
    value: u8,
) -> Result<WorkerAdministrativeState, StatusProtocolError> {
    match value {
        0 => Ok(WorkerAdministrativeState::Ready),
        1 => Ok(WorkerAdministrativeState::Drain),
        2 => Ok(WorkerAdministrativeState::Maintenance),
        _ => Err(StatusProtocolError::InvalidPayload),
    }
}

trait StatusEncoder {
    fn string(&mut self, value: &str) -> Result<(), StatusProtocolError>;

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), StatusProtocolError>;
}

impl StatusEncoder for BoundedWireWriter<StatusWire> {
    fn string(&mut self, value: &str) -> Result<(), StatusProtocolError> {
        if value.len() > MAX_STATUS_STRING_BYTES {
            return Err(StatusProtocolError::StringTooLong {
                maximum: MAX_STATUS_STRING_BYTES,
            });
        }
        self.u16(
            u16::try_from(value.len()).map_err(|_| StatusProtocolError::StringTooLong {
                maximum: MAX_STATUS_STRING_BYTES,
            })?,
        )?;
        self.bytes(value.as_bytes())
    }

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), StatusProtocolError> {
        match value {
            Some(value) => {
                self.u8(1)?;
                self.string(value)
            }
            None => self.u8(0),
        }
    }
}

trait StatusDecoder {
    fn flag(&mut self) -> Result<bool, StatusProtocolError>;

    fn string(&mut self) -> Result<String, StatusProtocolError>;

    fn optional_string(&mut self) -> Result<Option<String>, StatusProtocolError>;
}

impl StatusDecoder for BoundedWireReader<'_, StatusWire> {
    fn flag(&mut self) -> Result<bool, StatusProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StatusProtocolError::InvalidPayload),
        }
    }

    fn string(&mut self) -> Result<String, StatusProtocolError> {
        let length = usize::from(self.u16()?);
        if length > MAX_STATUS_STRING_BYTES {
            return Err(StatusProtocolError::StringTooLong {
                maximum: MAX_STATUS_STRING_BYTES,
            });
        }
        String::from_utf8(self.bytes(length)?.to_vec())
            .map_err(|_| StatusProtocolError::InvalidPayload)
    }

    fn optional_string(&mut self) -> Result<Option<String>, StatusProtocolError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.string().map(Some),
            _ => Err(StatusProtocolError::InvalidPayload),
        }
    }
}

struct StatusWire;

impl BoundedWireProtocol for StatusWire {
    type Error = StatusProtocolError;

    fn invalid() -> Self::Error {
        StatusProtocolError::InvalidPayload
    }

    fn too_large(_actual: usize, maximum: usize) -> Self::Error {
        StatusProtocolError::TooLarge { maximum }
    }

    fn allocation() -> Self::Error {
        StatusProtocolError::Allocation
    }
}

type Encoder = BoundedWireWriter<StatusWire>;
type Decoder<'a> = BoundedWireReader<'a, StatusWire>;

#[cfg(test)]
mod tests {
    use super::*;
    use oxiroute_supervision::{GenerationLaunchDocument, Revision};

    fn status() -> WorkerStatus {
        WorkerStatus {
            sequence: 1,
            generation_id: GenerationId(7),
            lifecycle: WorkerLifecycle::Active,
            administrative_state: WorkerAdministrativeState::Ready,
            accepting: true,
            runtime_started: true,
            runtime_failed: false,
            drained: false,
            generation: WorkerGenerationStatus {
                disk_revision: Some("a".repeat(64)),
                candidate_revision: None,
                active_revision: Some("b".repeat(64)),
                previous_revision: None,
                quarantined_revision: None,
                active_accepting: true,
                degraded: false,
                last_failure: None,
                prepares: 1,
                activations: 1,
                failures: 0,
                rollbacks: 0,
            },
            metrics: Some(WorkerMetrics {
                accepted_connections: 2,
                rejected_connections: 3,
                active_connections: 4,
                bytes_received: 5,
                bytes_sent: 6,
            }),
            listeners: vec![WorkerListenerStatus {
                name: "http".into(),
                protocol: "http".into(),
                bind: "127.0.0.1:8080".into(),
                administrative_state: WorkerAdministrativeState::Ready,
                state: WorkerListenerState::Listening,
                accepted_connections: 2,
                rejected_connections: 3,
                active_connections: 4,
                bytes_received: 5,
                bytes_sent: 6,
            }],
            degraded: false,
            degradation: None,
            event_cursor: 1,
            event_cursor_lost: false,
            events: vec![WorkerEventRecord {
                cursor: 1,
                timestamp_unix_ms: Some(9),
                event: "generation_activate".into(),
                outcome: "applied".into(),
                revision: Some("b".repeat(64)),
                certificate: None,
                correlation_id: Some("op-1".into()),
                source: Some("runtime".into()),
                operation: Some("generation_activate".into()),
            }],
        }
    }

    #[test]
    fn status_encoding_preserves_bounded_observation() {
        let expected = status();
        let payload = encode_status(&expected).expect("encode");
        assert!(!payload.is_empty());
        assert_eq!(expected.sequence, 1);
    }

    #[test]
    fn status_encoding_rejects_unbounded_event_batches() {
        let mut status = status();
        status.events = (1..=MAX_STATUS_EVENTS + 1)
            .map(|cursor| WorkerEventRecord {
                cursor: u64::try_from(cursor).expect("cursor"),
                timestamp_unix_ms: None,
                event: "event".into(),
                outcome: "applied".into(),
                revision: None,
                certificate: None,
                correlation_id: None,
                source: None,
                operation: None,
            })
            .collect();
        assert!(matches!(
            encode_status(&status),
            Err(StatusProtocolError::CollectionTooLong { kind: "events" })
        ));
    }

    #[test]
    fn status_prefix_and_tags_stay_byte_exact() {
        let payload = encode_status(&status()).expect("encode status");
        assert_eq!(
            &payload[..23],
            &[
                0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 3, 3, 0
            ]
        );
    }

    fn document(
        instance: &str,
        generation: u64,
        revision: u64,
    ) -> GenerationLaunchDocument<Revision, ()> {
        GenerationLaunchDocument::new(
            InstanceId::new(instance).unwrap(),
            GenerationId(generation),
            Revision(revision),
            (),
        )
    }

    fn replacement_catalog() -> SupervisedGenerationCatalog<Revision, ()> {
        let mut catalog = SupervisedGenerationCatalog::new(document("retired", 1, 10));
        catalog.begin_candidate(document("active", 2, 20)).unwrap();
        catalog.commit_candidate().unwrap();
        catalog
    }

    #[test]
    fn snapshot_separates_catalog_and_process_observations_and_sums_live_values() {
        let catalog = replacement_catalog();
        let mut active = status();
        active.generation_id = GenerationId(2);
        active.metrics = Some(WorkerMetrics {
            accepted_connections: 10,
            rejected_connections: 2,
            active_connections: 4,
            bytes_received: 100,
            bytes_sent: 200,
        });
        active.listeners[0].accepted_connections = 10;
        active.listeners[0].active_connections = 4;
        let mut retired = status();
        retired.generation_id = GenerationId(1);
        retired.metrics = Some(WorkerMetrics {
            accepted_connections: 7,
            rejected_connections: 1,
            active_connections: 2,
            bytes_received: 70,
            bytes_sent: 90,
        });
        retired.listeners[0].accepted_connections = 7;
        retired.listeners[0].active_connections = 2;
        retired.degraded = true;
        retired.degradation = Some("drain delayed".into());
        let active_id = InstanceId::new("active").unwrap();
        let retired_id = InstanceId::new("retired").unwrap();
        let mut history = SupervisorSnapshotHistory::new();
        history.record_events(
            GenerationRole::Active,
            &active_id,
            GenerationId(2),
            0,
            &active,
        );
        history.record_events(
            GenerationRole::Previous,
            &retired_id,
            GenerationId(1),
            0,
            &retired,
        );
        let sources = [
            WorkerSnapshotSource {
                role: GenerationRole::Active,
                instance_id: &active_id,
                generation_id: GenerationId(2),
                status: &active,
            },
            WorkerSnapshotSource {
                role: GenerationRole::Previous,
                instance_id: &retired_id,
                generation_id: GenerationId(1),
                status: &retired,
            },
        ];

        let snapshot = history.snapshot(&catalog, &sources).unwrap();

        assert_eq!(
            snapshot
                .generations
                .iter()
                .map(|generation| (generation.role, generation.revision))
                .collect::<Vec<_>>(),
            vec![
                (GenerationRole::Active, Revision(20)),
                (GenerationRole::Previous, Revision(10)),
            ]
        );
        assert_eq!(snapshot.processes.len(), 2);
        assert_eq!(snapshot.listener_observations.len(), 2);
        assert_eq!(snapshot.metrics.accepted_connections, 17);
        assert_eq!(snapshot.metrics.active_connections, 6);
        assert_eq!(snapshot.listeners[0].accepted_connections, 17);
        assert_eq!(snapshot.listeners[0].active_connections, 6);
        assert!(snapshot.degraded);
        assert_eq!(snapshot.degradation[0].role, GenerationRole::Previous);
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.events[0].role, GenerationRole::Active);
        assert_eq!(snapshot.events[1].role, GenerationRole::Previous);
    }

    #[test]
    fn reaped_workers_keep_cumulative_baselines_but_not_active_gauges() {
        let catalog = replacement_catalog();
        let mut active = status();
        active.generation_id = GenerationId(2);
        active.metrics = Some(WorkerMetrics {
            accepted_connections: 10,
            active_connections: 4,
            ..WorkerMetrics::default()
        });
        active.listeners[0].accepted_connections = 10;
        active.listeners[0].active_connections = 4;
        let mut retired = status();
        retired.generation_id = GenerationId(1);
        retired.metrics = Some(WorkerMetrics {
            accepted_connections: 7,
            active_connections: 2,
            ..WorkerMetrics::default()
        });
        retired.listeners[0].accepted_connections = 7;
        retired.listeners[0].active_connections = 2;
        let active_id = InstanceId::new("active").unwrap();
        let mut history = SupervisorSnapshotHistory::new();
        history.reap(Some(&retired));
        let sources = [WorkerSnapshotSource {
            role: GenerationRole::Active,
            instance_id: &active_id,
            generation_id: GenerationId(2),
            status: &active,
        }];

        let snapshot = history.snapshot(&catalog, &sources).unwrap();

        assert_eq!(snapshot.metrics.accepted_connections, 17);
        assert_eq!(snapshot.metrics.active_connections, 4);
        assert_eq!(snapshot.listeners[0].accepted_connections, 17);
        assert_eq!(snapshot.listeners[0].active_connections, 4);
        history.reap(Some(&active));
        let snapshot = history.snapshot(&catalog, &[]).unwrap();
        assert_eq!(snapshot.metrics.accepted_connections, 17);
        assert_eq!(snapshot.metrics.active_connections, 0);
        assert_eq!(snapshot.listeners[0].active_connections, 0);
    }

    #[test]
    fn snapshot_rejects_process_role_or_identity_mismatches() {
        let catalog = replacement_catalog();
        let active = status();
        let active_id = InstanceId::new("active").unwrap();
        let sources = [WorkerSnapshotSource {
            role: GenerationRole::Candidate,
            instance_id: &active_id,
            generation_id: GenerationId(2),
            status: &active,
        }];

        assert_eq!(
            SupervisorSnapshotHistory::new()
                .snapshot(&catalog, &sources)
                .unwrap_err(),
            SupervisorSnapshotError {
                instance_id: active_id,
                observed_role: GenerationRole::Candidate,
                catalog_role: Some(GenerationRole::Active),
            }
        );
    }

    #[test]
    fn status_counters_remain_monotonic_while_gauges_use_the_latest_value() {
        let previous = status();
        let mut current = status();
        current.metrics = Some(WorkerMetrics {
            accepted_connections: 1,
            rejected_connections: 1,
            active_connections: 1,
            bytes_received: 1,
            bytes_sent: 1,
        });
        current.listeners[0].accepted_connections = 1;
        current.listeners[0].active_connections = 1;
        current.generation.activations = 0;

        retain_monotonic_status(&previous, &mut current);

        let metrics = current.metrics.unwrap();
        assert_eq!(metrics.accepted_connections, 2);
        assert_eq!(metrics.active_connections, 1);
        assert_eq!(current.listeners[0].accepted_connections, 2);
        assert_eq!(current.listeners[0].active_connections, 1);
        assert_eq!(current.generation.activations, 1);
    }

    #[test]
    fn aggregated_events_are_bounded_with_stable_master_cursors() {
        let mut history = SupervisorSnapshotHistory::new();
        let instance_id = InstanceId::new("active").unwrap();
        for cursor in 1..=u64::try_from(MAX_AGGREGATED_EVENTS + 3).unwrap() {
            let mut current = status();
            current.event_cursor = cursor;
            current.events[0].cursor = cursor;
            history.record_events(
                GenerationRole::Active,
                &instance_id,
                GenerationId(7),
                cursor - 1,
                &current,
            );
        }

        let events = history.worker_events(0, usize::MAX);
        assert_eq!(events.len(), MAX_AGGREGATED_EVENTS);
        assert_eq!(events.first().unwrap().cursor, 4);
        assert_eq!(
            events.last().unwrap().cursor,
            u64::try_from(MAX_AGGREGATED_EVENTS + 3).unwrap()
        );
        assert!(
            events
                .windows(2)
                .all(|events| events[0].cursor < events[1].cursor)
        );

        history.next_event_cursor = u64::MAX;
        let mut final_status = status();
        final_status.event_cursor = 2;
        final_status.events[0].cursor = 2;
        history.record_events(
            GenerationRole::Active,
            &instance_id,
            GenerationId(7),
            1,
            &final_status,
        );
        final_status.event_cursor = 3;
        final_status.events[0].cursor = 3;
        history.record_events(
            GenerationRole::Active,
            &instance_id,
            GenerationId(7),
            2,
            &final_status,
        );
        let events = history.worker_events(0, usize::MAX);
        assert_eq!(events.last().unwrap().cursor, u64::MAX);
        assert!(
            events
                .windows(2)
                .all(|events| events[0].cursor < events[1].cursor)
        );
    }
}
