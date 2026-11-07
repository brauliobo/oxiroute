use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque, hash_map::Entry},
    error::Error,
    fmt, io,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

#[cfg(target_os = "linux")]
use std::fs;

use serde::Serialize;

use oxiroute_config::ListenerBind;

use crate::listener::inventory::{ListenerEntry, ListenerId, ListenerMetricPolicy};
use crate::{
    AcmeManagedReconciler, AdministrativeState, CertbotReconciler, CertbotWatcherMonitor,
    FileReconciler, FileWatcherMonitor, PoolHealthSnapshot, ProxyProtocolResult, RoundRobinPool,
};

mod counters;
mod error;
mod listener;
mod outcomes;
mod runtime;
mod runtime_snapshot;
mod snapshots;
mod system;

use system::{CpuSample, SystemSample, cpu_percent, sample_system, unix_time_ms};

pub(crate) use counters::*;
pub use error::*;
pub use listener::*;
pub use outcomes::*;
pub use runtime::*;
pub use snapshots::*;

#[cfg(test)]
mod platform_tests;

#[cfg(all(test, target_os = "linux"))]
mod tests;
