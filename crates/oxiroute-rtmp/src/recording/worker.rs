use std::{
    collections::VecDeque,
    io::{self, Seek, SeekFrom, Write},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    FlvMuxer, MediaEvent, MediaEventKind, RecorderLease, RecordingCommit, RecordingDateTime,
    RecordingFile, RecordingPathPolicy, RecordingStore, RecordingStoreError, RecordingTimeBasis,
    recording::store::{
        FinalizerTicket, MAX_PENDING_FINALIZATIONS_PER_RECORDER, RecordingCommitCancellation,
    },
};

mod config;
mod context;
mod finalization;
mod runner;
mod segment;
mod shared;
mod status;

pub use config::*;
#[allow(clippy::wildcard_imports)]
use context::*;
#[allow(clippy::wildcard_imports)]
use finalization::*;
pub use runner::*;
#[allow(clippy::wildcard_imports)]
use segment::*;
#[allow(clippy::wildcard_imports)]
use shared::*;

pub use status::{
    RecorderEnqueueResult, RecorderFailure, RecorderNotification, RecorderWorkerPhase,
    RecorderWorkerStartError, RecorderWorkerStatus,
};

const TIMESTAMP_HALF_RANGE: u32 = 1 << 31;
const MAX_ROTATION_INTERVAL_MS: u128 = (TIMESTAMP_HALF_RANGE - 1) as u128;

#[cfg(test)]
mod tests;
