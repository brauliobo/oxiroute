//! Deterministic socket-owning orchestration for replaceable supervised workers.
//!
//! The master retains every original listener descriptor. Workers receive short-lived duplicates
//! over the authenticated process channel and validate them against the stable manifest before
//! acknowledging adoption. Post-spawn I/O is driven only by [`Master::poll`]; this crate starts no
//! threads and integrates with no async runtime or server.

#[cfg(not(target_os = "linux"))]
compile_error!("oxiroute-supervisor-master requires Linux");

mod config;
mod listeners;
mod master;
mod protocol;
mod status;

pub use config::{ConfigError, MasterConfig};
pub use listeners::{ListenerOwnershipError, StableListeners};
pub use master::{
    ActionError, ActionExecutor, ActionKind, FailurePhase, Master, MasterError, MasterEvent,
    MasterState, PreparationError, PreparationStep, ShutdownProgress, SystemActionExecutor,
    WorkerFactory, WorkerInput, WorkerRole, WorkerState,
};
pub use protocol::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, ControlProtocolError, ControlRequest,
    DESCRIPTOR_MANIFEST_VERSION, MAX_MANIFEST_BYTES, SUPPORTED_DESCRIPTOR_CAPABILITIES,
    WorkerControl,
};
pub use status::{
    AggregatedWorkerEvent, MAX_AGGREGATED_EVENTS, MAX_STATUS_BYTES, MAX_STATUS_EVENTS,
    MAX_STATUS_LISTENERS, StatusProtocolError, SupervisorDegradation, SupervisorEventRecord,
    SupervisorGenerationSnapshot, SupervisorListenerObservation, SupervisorListenerSnapshot,
    SupervisorProcessSnapshot, SupervisorSnapshot, SupervisorSnapshotError,
    WorkerAdministrativeState, WorkerEventRecord, WorkerGenerationStatus, WorkerLifecycle,
    WorkerListenerState, WorkerListenerStatus, WorkerMetrics, WorkerStatus,
};
