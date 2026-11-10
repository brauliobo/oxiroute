use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::File,
    io::{Read as _, Write as _},
    os::fd::OwnedFd,
    path::{Component, Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use oxiroute_config::Audit;
use rustix::{
    fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags, RenameFlags},
    io::Errno,
};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};
use tokio::sync::Notify;

use crate::EvacuationReport;
use crate::config::coordinator::EffectiveRevision;
use crate::observability::logging::{
    next_correlation_id, redact_identifier, redact_text, valid_correlation_id,
};
use crate::observability::monitoring::{ObservedTransport, TransportOutcome};
use crate::routing::{EndpointHealthState, HealthFailure};

pub(crate) const EVENT_CAPACITY: usize = 2_048;
const AUDIT_ACTIVE_FILE: &str = "audit.ndjson";
const AUDIT_TEMP_FILE: &str = "audit.ndjson.tmp";
const AUDIT_ROOT_MODE: u32 = 0o700;
const AUDIT_FILE_MODE: u32 = 0o600;
const MAX_AUDIT_FILE_BYTES: u64 = Audit::MAX_FILE_BYTES;
#[allow(clippy::cast_possible_truncation)] // 16 MiB fits every target's usize
const AUDIT_SCAN_LIMIT_BYTES: usize = MAX_AUDIT_FILE_BYTES as usize + 1;
const CORRELATION_ID_MAX_BYTES: usize = 64;

mod audit;
mod audit_store;
mod event;
mod event_log;

pub(crate) use audit::*;
pub(crate) use audit_store::*;
pub(crate) use event::*;
pub use event_log::*;

#[cfg(test)]
mod audit_batching_tests;
#[cfg(test)]
mod tests;
