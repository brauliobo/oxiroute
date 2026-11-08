use std::{
    collections::VecDeque,
    error::Error,
    fmt, io,
    os::fd::{AsFd as _, BorrowedFd, OwnedFd},
    process::ExitStatus,
    time::{Duration, Instant},
};

use oxiroute_supervision::{
    GenerationId, GenerationRole, Instance, InstanceId, Lifecycle, ReplacementAction,
    ReplacementError, ReplacementEvent, ReplacementPhase, ReplacementSupervisor,
    SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::FrameFlags;
use oxiroute_supervisor_process::{
    AuthenticatedChannelError, SpawnError, WorkerCommand, WorkerEvent, WorkerIdentity,
    WorkerProcess, WorkerSpawner,
};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use thiserror::Error;

use crate::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, ControlProtocolError, MasterConfig,
    StableListeners,
    listeners::ListenerOwnershipError,
    protocol::{
        ControlAck, MAX_ATTACHMENT_BYTES, decode_ack, encode_adopt_request, encode_request,
    },
    status::{
        AggregatedWorkerEvent, SupervisorSnapshot, SupervisorSnapshotError,
        SupervisorSnapshotHistory, WorkerSnapshotSource, WorkerStatus, decode_status,
        retain_monotonic_status,
    },
};

mod observation;

use observation::{Observation, Observed};

mod dispatch;
mod executor;
mod failure;
mod model;
mod query;
mod replacement;
mod runtime;

pub use executor::*;
pub use model::*;
pub use runtime::*;
