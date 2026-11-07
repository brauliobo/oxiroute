use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags},
    io::Errno,
};
use uuid::Uuid;

use crate::{
    MAX_RECORDING_FILENAME_BYTES, RtmpPrepareMode, recording::path::collision_recording_filename,
};

mod commit;
mod error;
mod file;
mod finalizer;
mod fsutil;
mod limits;
mod registry;
mod store;

pub use commit::*;
pub use error::*;
pub use file::*;
pub(crate) use finalizer::*;
#[allow(clippy::wildcard_imports)]
use fsutil::*;
pub use limits::*;
pub use registry::*;
pub use store::*;

mod flv;

use flv::inspect_tail as inspect_flv_tail;

const PARTIAL_PREFIX: &str = ".oxiroute-recording-";
const PARTIAL_SUFFIX: &str = ".partial";
const OWNERSHIP_LOCK_NAME: &str = ".oxiroute-recording.lock";
const OWNER_PROBE_PREFIX: &str = ".oxiroute-owner-probe-";
const UUID_SIMPLE_LENGTH: usize = 32;
const MAX_NAME_ATTEMPTS: usize = 16;
const MAX_FINALIZER_THREADS: usize = 1;
pub(crate) const MAX_PENDING_FINALIZATIONS_PER_RECORDER: usize = 2;
const OWNERSHIP_RETRY_INTERVAL: Duration = Duration::from_millis(2);

#[cfg(test)]
mod tests;
