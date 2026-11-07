#[allow(clippy::wildcard_imports)]
use super::*;

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
