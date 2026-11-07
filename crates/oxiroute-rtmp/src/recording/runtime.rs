use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod cleanup;
mod controller;
mod policy;
mod reaper;

#[allow(clippy::wildcard_imports)]
use cleanup::*;
pub(crate) use controller::*;
pub use policy::{RtmpRecorderPolicy, RtmpRecorderStart};
pub use reaper::*;

use crate::{
    MediaEvent, MediaEventKind, OperationId, RecorderEnqueueResult, RecorderErrorCode,
    RecorderFailure, RecorderId, RecorderWorker, RecorderWorkerConfig, RecorderWorkerPhase,
    RecorderWorkerStatus, RecorderWorkerSupervisor, RecordingDateTime, RtmpRegistry, SessionId,
    StreamId,
};

const REAPER_POLL_INTERVAL: Duration = Duration::from_millis(2);
#[cfg(test)]
const CONTINUOUS_RESTART_DELAY: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const CONTINUOUS_RESTART_DELAY: Duration = Duration::from_millis(250);

#[cfg(test)]
mod tests;
