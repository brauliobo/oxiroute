//! Platform-neutral primitives for supervising replaceable service processes.
//!
//! This crate contains only deterministic value types and state machines. It performs no I/O,
//! spawns no tasks, and depends on no async runtime.

mod bounded;
mod catalog;
mod control;
mod correlation;
mod identity;
mod lifecycle;
mod metrics;
mod protocol;
mod replacement;
mod snapshot;
mod validated;
mod wire;

pub use bounded::{BoundError, BoundedString, BoundedVec};
pub use catalog::{
    CatalogError, GenerationLaunchDocument, GenerationRole, SupervisedGenerationCatalog,
};
pub use control::{LifecycleControl, LifecycleOperation, LifecycleRequest};
pub use correlation::{
    CorrelationError, CorrelationGeneration, CorrelationTable, CorrelationToken, PendingRequest,
};
pub use identity::{
    Epoch, GenerationId, IdentityError, InstanceId, RequestId, Revision, Sequence, ServiceId,
};
pub use lifecycle::{Lifecycle, TransitionError};
pub use metrics::{
    MetricBatch, MetricBatchError, MetricDescriptor, MetricId, MetricKind, MetricRegistry,
    MetricRegistryError, MetricSample, MetricValue,
};
pub use protocol::{
    EventEnvelope, MessageEnvelope, ProtocolError, RequestEnvelope, ResponseEnvelope, RpcOutcome,
    ServiceProtocol,
};
pub use replacement::{
    Instance, ReplacementAction, ReplacementError, ReplacementEvent, ReplacementPhase,
    ReplacementSupervisor,
};
pub use snapshot::{SnapshotEnvelope, SnapshotError};
pub use wire::{BoundedWireProtocol, BoundedWireReader, BoundedWireWriter};
