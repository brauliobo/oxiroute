use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags},
    io::Errno,
};
use uuid::Uuid;

use crate::{
    BaseKey, Cache, CacheConfig, CacheError, CacheKey, CacheStats, CachedResponse, Clock,
    FillGuard, FillJoin, FillWaiter, Lookup, MonoTime, PreparedEntry, PurgeResult, RequestKeyInput,
    ResponseTiming, StoreOutcome, SystemClock, Validators,
    cache::{ClaimedStoreOutcome, InsertionDelta, RecoveredEntry, object_charge, validate_tag},
    key::VaryValue,
    policy::{ResponsePolicy, RetentionPolicy},
};

mod codec;

use codec::{Decoder, Encoder};

const MAGIC: &[u8; 8] = b"OXICACHE";
const RECORD_VERSION: u16 = 2;
const HEADER_BYTES: usize = 8 + 2 + 8 + 4;
const ROOT_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const LOCK_NAME: &str = ".oxiroute-cache.lock";
const TEMP_PREFIX: &str = ".oxiroute-cache-";
const TEMP_SUFFIX: &str = ".tmp";
const RECORD_PREFIX: &str = ".oxiroute-cache-";
const RECORD_SUFFIX: &str = ".record";
const OWNER_PROBE_PREFIX: &str = ".oxiroute-cache-owner-";
const MAX_NAME_ATTEMPTS: usize = 16;

mod config;
mod record;
mod root;
mod shared;
mod store;

pub use config::*;
#[allow(clippy::wildcard_imports)]
use record::*;
#[allow(clippy::wildcard_imports)]
use root::*;
#[allow(clippy::wildcard_imports)]
use shared::*;
pub use store::*;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    AfterFileSync,
    AfterPublish,
    BeforeDirectorySync,
    BeforeMemoryReconciliation,
}

#[cfg(test)]
mod tests;
