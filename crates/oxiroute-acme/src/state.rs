use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(not(unix))]
use std::fs::OpenOptions;

use rustix::fs::{self as rustix_fs, FileType, FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

pub const MAX_STATE_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CERTIFICATE_BYTES: usize = 1024 * 1024;
pub const MAX_JOB_BYTES: usize = 64 * 1024;

const MAX_SLUG_BYTES: usize = 128;
const MAX_JOB_ATTEMPTS: u32 = 64;
const ROOT_MODE: u32 = 0o700;
const SECRET_MODE: u32 = 0o600;
const PUBLIC_MODE: u32 = 0o644;
const MAX_TEMP_ATTEMPTS: usize = 128;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SHARED_LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<File>>>> = OnceLock::new();

mod files;
mod revision;
mod store;
mod types;
mod validate;

#[allow(clippy::wildcard_imports)]
use files::*;
pub use revision::*;
pub use store::*;
pub use types::*;
#[allow(clippy::wildcard_imports)]
use validate::*;

#[cfg(test)]
mod tests;
