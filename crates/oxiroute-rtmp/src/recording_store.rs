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
    MAX_RECORDING_FILENAME_BYTES, RtmpPrepareMode, recording_path::collision_recording_filename,
};

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

/// Optional storage quotas and the hard active-recorder bound for one pinned recording root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingStoreLimits {
    pub max_bytes: Option<u64>,
    pub max_files: Option<usize>,
    pub max_active_recorders: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("recording store limit `{field}` must be nonzero when configured")]
pub struct RecordingStoreLimitsError {
    pub field: &'static str,
}

impl RecordingStoreLimits {
    /// Validates quota values without opening or inspecting a recording root.
    ///
    /// # Errors
    ///
    /// Returns an error when the active-recorder bound cannot initialize the store finalizer.
    pub fn validate_intrinsic(self) -> Result<(), RecordingStoreLimitsError> {
        if self.max_active_recorders == 0 {
            return Err(RecordingStoreLimitsError {
                field: "max_active_recorders",
            });
        }
        Ok(())
    }
}

/// Current use under one recording store.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecordingStoreStats {
    pub bytes_used: u64,
    pub files: usize,
    pub active_recorders: usize,
}

/// Quota counters are shared by stores in this process. The lock protocol protects partial cleanup
/// across processes, but quota counters are not distributed between daemon processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingQuotaScope {
    Process,
}

/// One durably published recording. The name is always relative and one component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingCommit {
    pub relative_name: String,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingStoreError {
    #[error("recording root cannot be opened as a no-follow directory")]
    RootOpen(#[source] io::Error),
    #[error("recording root cannot be enumerated")]
    RootRead(#[source] io::Error),
    #[error("recording root entry cannot be inspected")]
    RootEntryMetadata(#[source] io::Error),
    #[error("recording root must be daemon-owned and not writable by group or other users")]
    RootNotExclusive,
    #[error("recording root ownership lease cannot be opened")]
    OwnershipOpen(#[source] io::Error),
    #[error("recording root ownership lease cannot be acquired")]
    OwnershipLock(#[source] io::Error),
    #[error("recording creation was cancelled while waiting for storage ownership")]
    CreationCancelled,
    #[error("owned recording partial cannot be removed")]
    PartialCleanup(#[source] io::Error),
    #[error("recording root cannot be synchronized")]
    RootSync(#[source] io::Error),
    #[error(
        "recording root already uses {bytes_used} bytes and {files} files, exceeding configured limits"
    )]
    ExistingUsageExceedsLimits { bytes_used: u64, files: usize },
    #[error("recording root is already open in this process with different limits")]
    LimitsMismatch,
    #[error("recording finalizer thread cannot be started")]
    FinalizerThreadSpawn(#[source] io::Error),
    #[error("recording final name must be a bounded relative path component")]
    InvalidRelativeName,
    #[error("recording active-recorder limit of {maximum} reached")]
    ActiveRecorderLimit { maximum: usize },
    #[error("recording file limit of {maximum} reached")]
    FileLimit { maximum: usize },
    #[error("recording partial-name collision retry limit reached")]
    PartialNameCollisions,
    #[error("recording partial cannot be created")]
    PartialCreate(#[source] io::Error),
    #[error("existing recording cannot be securely reopened")]
    ResumeOpen(#[source] io::Error),
    #[error("existing recording is not a complete FLV stream")]
    ResumeInvalid,
    #[error("recording partial {partial_relative_name} cannot be flushed or synchronized")]
    FileSync {
        partial_relative_name: String,
        #[source]
        source: io::Error,
    },
    #[error("recording partial ownership was lost before publication")]
    PartialOwnershipLost { partial_relative_name: String },
    #[error("recording finalization was cancelled before publication")]
    FinalizationCancelled { partial_relative_name: String },
    #[error("recording partial cannot be atomically published")]
    Publish {
        partial_relative_name: String,
        #[source]
        source: io::Error,
    },
    #[error("recording partial unlink failed and the published recording could not be rolled back")]
    PublishRollback {
        partial_relative_name: String,
        recording: RecordingCommit,
        #[source]
        source: io::Error,
        rollback_source: io::Error,
    },
    #[error("recording rollback could not be synchronized after the final name was removed")]
    PublishRollbackDirectorySync {
        partial_relative_name: String,
        recording: RecordingCommit,
        #[source]
        source: io::Error,
        directory_sync_source: io::Error,
    },
    #[error("descriptor-based recording publication is unsupported")]
    DescriptorPublicationUnsupported { partial_relative_name: String },
    #[error("recording final-name collision retry limit reached")]
    FinalNameCollisions { partial_relative_name: String },
    #[error("recording was published but its directory cannot be synchronized")]
    PublishedDirectorySync {
        recording: RecordingCommit,
        #[source]
        source: io::Error,
    },
}

impl RecordingStoreError {
    #[must_use]
    pub fn recoverable_partial_name(&self) -> Option<&str> {
        match self {
            Self::FileSync {
                partial_relative_name,
                ..
            }
            | Self::FinalizationCancelled {
                partial_relative_name,
            }
            | Self::Publish {
                partial_relative_name,
                ..
            }
            | Self::PublishRollback {
                partial_relative_name,
                ..
            }
            | Self::PublishRollbackDirectorySync {
                partial_relative_name,
                ..
            }
            | Self::DescriptorPublicationUnsupported {
                partial_relative_name,
            }
            | Self::FinalNameCollisions {
                partial_relative_name,
            } => Some(partial_relative_name),
            _ => None,
        }
    }

    #[must_use]
    pub const fn published_recording(&self) -> Option<&RecordingCommit> {
        match self {
            Self::PublishRollback { recording, .. }
            | Self::PublishRollbackDirectorySync { recording, .. }
            | Self::PublishedDirectorySync { recording, .. } => Some(recording),
            _ => None,
        }
    }
}

/// A recording root opened once and retained by descriptor for its full lifetime.
#[derive(Clone)]
pub struct RecordingStore {
    handle: Arc<RecordingStoreHandle>,
}

/// Deduplicates recording-store preparation for one RTMP generation.
#[derive(Default)]
pub struct RtmpRecorderStoreRegistry {
    stores: HashMap<PathBuf, PreparedRecordingStore>,
}

struct PreparedRecordingStore {
    limits: RecordingStoreLimits,
    store: Option<RecordingStore>,
}

impl RtmpRecorderStoreRegistry {
    /// Preflights or opens one recording root according to the preparation mode.
    ///
    /// Validation scans each root once without opening a store. Activation reuses one opened store
    /// for every recorder that names the same root. Activation requires equal limits for every use
    /// of one root, matching the underlying process-wide recording store.
    ///
    /// # Errors
    ///
    /// Returns the underlying recording-store error when limits conflict or preparation fails.
    pub fn prepare(
        &mut self,
        root: impl AsRef<Path>,
        limits: RecordingStoreLimits,
        mode: RtmpPrepareMode,
        deadline: Option<Instant>,
    ) -> Result<Option<RecordingStore>, RecordingStoreError> {
        let root = root.as_ref();
        if let Some(prepared) = self.stores.get(root) {
            if mode == RtmpPrepareMode::Validation {
                return Ok(None);
            }
            if prepared.limits != limits {
                return Err(RecordingStoreError::LimitsMismatch);
            }
            if let Some(store) = &prepared.store {
                return Ok(Some(store.clone()));
            }
        }

        if mode == RtmpPrepareMode::Validation {
            RecordingStore::preflight(root, limits)?;
            self.stores.insert(
                root.to_owned(),
                PreparedRecordingStore {
                    limits,
                    store: None,
                },
            );
            return Ok(None);
        }

        let store = RecordingStore::open_with_deadline(root, limits, deadline)?;
        self.stores.insert(
            root.to_owned(),
            PreparedRecordingStore {
                limits,
                store: Some(store.clone()),
            },
        );
        Ok(Some(store))
    }
}

struct RecordingStoreHandle {
    incarnation: u64,
    shared: Mutex<Option<Arc<StoreShared>>>,
}

struct StoreShared {
    root: OwnedFd,
    root_owner: u32,
    limits: RecordingStoreLimits,
    state: Mutex<StoreState>,
    incarnation_changed: Condvar,
    finalizer: RecordingFinalizer,
    #[cfg(test)]
    operation_gate: Mutex<Option<Arc<RecordingOperationGate>>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IncarnationState {
    active: bool,
    claims: usize,
}

#[derive(Clone, Debug, Default)]
struct StoreState {
    bytes_used: u64,
    files: usize,
    active_recorders: usize,
    incarnations: HashMap<u64, IncarnationState>,
}

struct StoreOperationClaim {
    incarnation: u64,
    shared: Arc<StoreShared>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingOperationGateKind {
    CreateBeforeOpen,
    RegisterBeforeWait,
    RegisterAfterWake,
    ResumeAfterSharedCapture,
}

#[cfg(test)]
struct RecordingOperationGate {
    state: Mutex<RecordingOperationGateState>,
    changed: Condvar,
}

#[cfg(test)]
struct RecordingOperationGateState {
    kind: RecordingOperationGateKind,
    reached: bool,
    released: bool,
}

type FinalizerJob = Box<dyn FnOnce() + Send + 'static>;

struct FinalizerJobEntry {
    id: u64,
    job: FinalizerJob,
}

struct RecordingFinalizer {
    shared: Arc<FinalizerShared>,
    threads: Vec<JoinHandle<()>>,
}

struct FinalizerShared {
    state: Mutex<FinalizerState>,
    available: Condvar,
    space_available: Condvar,
    queue_capacity: usize,
}

#[derive(Default)]
struct FinalizerState {
    jobs: VecDeque<FinalizerJobEntry>,
    next_job_id: u64,
    stopping: bool,
}

pub(crate) struct FinalizerTicket {
    shared: Weak<FinalizerShared>,
    id: u64,
}

/// One active recorder slot retained for a worker's full lifetime.
pub struct RecorderLease {
    shared: Arc<StoreShared>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

impl RecordingStore {
    /// Validates a recording root and its existing quota use without mutating the filesystem.
    ///
    /// This does not acquire the ownership lease, create an owner probe, remove partials, or sync the
    /// directory. [`Self::open`] remains the descriptor-pinned mutating startup operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be opened or enumerated, is not exclusively controlled
    /// by the current daemon user, or already exceeds the supplied quota.
    pub fn preflight(
        root: impl AsRef<Path>,
        limits: RecordingStoreLimits,
    ) -> Result<RecordingStoreStats, RecordingStoreError> {
        let root = open_pinned_directory(root.as_ref())
            .map_err(|source| RecordingStoreError::RootOpen(source.into()))?;
        verify_root_control_read_only(&root)?;
        let state = scan_root(&root, limits, false)?;
        Ok(RecordingStoreStats {
            bytes_used: state.bytes_used,
            files: state.files,
            active_recorders: 0,
        })
    }

    /// Opens and pins `root` one component at a time without following symlinks.
    ///
    /// Existing regular files count toward the configured root quotas. Exact regular files in the
    /// store's hidden partial namespace are removed only while an exclusive ownership lease proves
    /// no writer is active. Similarly named files, symlinks, directories, and other entries are
    /// never cleaned up.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be pinned, enumerated, cleaned, synchronized, or is
    /// already over quota.
    pub fn open(
        root: impl AsRef<Path>,
        limits: RecordingStoreLimits,
    ) -> Result<Self, RecordingStoreError> {
        Self::open_with_deadline(root, limits, None)
    }

    /// Opens a store while bounding waits for a retired incarnation to release its claims.
    ///
    /// `None` preserves the unbounded startup behavior of [`Self::open`].
    #[doc(hidden)]
    pub fn open_with_deadline(
        root: impl AsRef<Path>,
        limits: RecordingStoreLimits,
        deadline: Option<Instant>,
    ) -> Result<Self, RecordingStoreError> {
        let root = open_pinned_directory(root.as_ref())
            .map_err(|source| RecordingStoreError::RootOpen(source.into()))?;
        let root_metadata = verify_root_control(&root)?;
        let root_owner = root_metadata.st_uid;
        let identity = RootIdentity {
            device: root_metadata.st_dev,
            inode: root_metadata.st_ino,
        };
        let existing = {
            let registry = store_registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.get(&identity).and_then(Weak::upgrade)
        };
        if let Some(shared) = existing {
            if shared.limits != limits {
                return Err(RecordingStoreError::LimitsMismatch);
            }
            return Self::from_shared(shared, deadline);
        }
        let clean_partials = match rustix_fs::flock(&root, FlockOperation::NonBlockingLockExclusive)
        {
            Ok(()) => true,
            Err(source) if source == Errno::AGAIN || source == Errno::WOULDBLOCK => {
                rustix_fs::flock(&root, FlockOperation::LockShared)
                    .map_err(|source| RecordingStoreError::OwnershipLock(source.into()))?;
                false
            }
            Err(source) => return Err(RecordingStoreError::OwnershipLock(source.into())),
        };
        let state = scan_root(&root, limits, clean_partials)?;
        rustix_fs::flock(&root, FlockOperation::Unlock)
            .map_err(|source| RecordingStoreError::OwnershipLock(source.into()))?;
        let finalizer = RecordingFinalizer::new(limits.max_active_recorders)?;
        let shared = Arc::new(StoreShared {
            root,
            root_owner,
            limits,
            state: Mutex::new(state),
            incarnation_changed: Condvar::new(),
            finalizer,
            #[cfg(test)]
            operation_gate: Mutex::new(None),
        });
        let shared = {
            let mut registry = store_registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = registry.get(&identity).and_then(Weak::upgrade) {
                existing
            } else {
                registry.insert(identity, Arc::downgrade(&shared));
                shared
            }
        };
        if shared.limits != limits {
            return Err(RecordingStoreError::LimitsMismatch);
        }
        Self::from_shared(shared, deadline)
    }

    /// Creates one exclusive recording path for a validated final relative name.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid name, exhausted quota, repeated final-name collisions, or
    /// an operating-system creation failure.
    pub fn create(&self, final_relative_name: &str) -> Result<RecordingFile, RecordingStoreError> {
        self.create_inner(final_relative_name, || false, false, None)
    }

    /// Reserves one active recorder slot until the returned lease is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the root's active-recorder limit has been reached.
    pub fn acquire_recorder(&self) -> Result<RecorderLease, RecordingStoreError> {
        let shared = self.shared()?;
        let mut state = shared.lock();
        if !state
            .incarnations
            .get(&self.handle.incarnation)
            .is_some_and(|incarnation| incarnation.active)
        {
            return Err(RecordingStoreError::CreationCancelled);
        }
        if state.active_recorders >= shared.limits.max_active_recorders {
            return Err(RecordingStoreError::ActiveRecorderLimit {
                maximum: shared.limits.max_active_recorders,
            });
        }
        state.active_recorders += 1;
        drop(state);
        Ok(RecorderLease { shared })
    }

    pub(crate) fn create_unless_with_options(
        &self,
        final_relative_name: &str,
        cancelled: impl Fn() -> bool,
        lock: bool,
        max_bytes: Option<u64>,
    ) -> Result<RecordingFile, RecordingStoreError> {
        self.create_inner(final_relative_name, cancelled, lock, max_bytes)
    }

    fn create_inner(
        &self,
        final_relative_name: &str,
        cancelled: impl Fn() -> bool,
        lock: bool,
        max_bytes: Option<u64>,
    ) -> Result<RecordingFile, RecordingStoreError> {
        validate_relative_name(final_relative_name)?;
        let shared = self.shared()?;
        let ownership = acquire_shared_ownership(&shared.root, &cancelled)?;
        if cancelled() {
            return Err(RecordingStoreError::CreationCancelled);
        }
        let claim = shared
            .claim(self.handle.incarnation)
            .ok_or(RecordingStoreError::CreationCancelled)?;
        #[cfg(test)]
        shared.wait_at_operation_gate(RecordingOperationGateKind::CreateBeforeOpen);
        {
            let mut state = shared.lock();
            if shared
                .limits
                .max_files
                .is_some_and(|maximum| state.files >= maximum)
            {
                return Err(RecordingStoreError::FileLimit {
                    maximum: shared.limits.max_files.expect("checked file quota"),
                });
            }
            state.files += 1;
        }

        let (descriptor, relative_name) =
            match create_recording_entry(&shared, &claim, final_relative_name, lock) {
                Ok(created) => created,
                Err(error) => {
                    if !matches!(error, RecordingStoreError::PartialCleanup(_)) {
                        shared.lock().files -= 1;
                    }
                    return Err(error);
                }
            };
        drop(claim);
        Ok(RecordingFile {
            incarnation: self.handle.incarnation,
            shared,
            _ownership: ownership,
            file: Some(File::from(descriptor)),
            partial_name: relative_name.clone(),
            final_relative_name: relative_name,
            position: 0,
            length: 0,
            partial_exists: true,
            partial_exists_state: Arc::new(AtomicBool::new(true)),
            file_accounted: true,
            max_bytes,
            preserve_partial: Arc::new(AtomicBool::new(false)),
            commit: Arc::new(RecordingCommitState {
                state: AtomicU8::new(CommitPhase::Open.encode()),
                #[cfg(test)]
                gate: Mutex::new(None),
                #[cfg(test)]
                fail_partial_unlink: AtomicBool::new(false),
                #[cfg(test)]
                fail_rollback_unlink: AtomicBool::new(false),
                #[cfg(test)]
                fail_rollback_sync: AtomicBool::new(false),
            }),
            resumed: false,
        })
    }

    #[must_use]
    pub fn stats(&self) -> RecordingStoreStats {
        let Ok(shared) = self.shared() else {
            return RecordingStoreStats::default();
        };
        let state = shared.lock();
        RecordingStoreStats {
            bytes_used: state.bytes_used,
            files: state.files,
            active_recorders: state.active_recorders,
        }
    }

    pub(crate) fn recording_names(&self) -> Result<Vec<String>, RecordingStoreError> {
        let shared = self.shared()?;
        let mut directory = Dir::read_from(&shared.root)
            .map_err(|source| RecordingStoreError::RootRead(source.into()))?;
        let mut names = Vec::new();
        for entry in &mut directory {
            let entry = entry.map_err(|source| RecordingStoreError::RootRead(source.into()))?;
            let name = entry.file_name();
            let bytes = name.to_bytes();
            if bytes.starts_with(b".") {
                continue;
            }
            let metadata = match rustix_fs::statat(&shared.root, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(Errno::NOENT) => continue,
                Err(source) => {
                    return Err(RecordingStoreError::RootEntryMetadata(source.into()));
                }
            };
            if FileType::from_raw_mode(metadata.st_mode).is_file()
                && let Ok(name) = std::str::from_utf8(bytes)
            {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    pub(crate) fn resume_with_options(
        &self,
        relative_name: &str,
        lock: bool,
        max_bytes: Option<u64>,
    ) -> Result<RecordingResume, RecordingStoreError> {
        validate_relative_name(relative_name)?;
        let shared = self.shared()?;
        #[cfg(test)]
        shared.wait_at_operation_gate(RecordingOperationGateKind::ResumeAfterSharedCapture);
        let claim = shared
            .claim(self.handle.incarnation)
            .ok_or(RecordingStoreError::CreationCancelled)?;
        let ownership = acquire_shared_ownership(&shared.root, &|| false)?;
        let descriptor = rustix_fs::openat(
            &shared.root,
            relative_name,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        let metadata = rustix_fs::fstat(&descriptor)
            .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        let path_metadata =
            rustix_fs::statat(&shared.root, relative_name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file()
            || metadata.st_uid != shared.root_owner
            || metadata.st_nlink != 1
            || metadata.st_dev != path_metadata.st_dev
            || metadata.st_ino != path_metadata.st_ino
        {
            return Err(RecordingStoreError::ResumeOpen(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording entry identity is invalid",
            )));
        }
        if lock {
            rustix_fs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive)
                .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        }
        let length =
            u64::try_from(metadata.st_size).map_err(|_| RecordingStoreError::ResumeInvalid)?;
        if max_bytes.is_some_and(|maximum| length > maximum) {
            return Err(RecordingStoreError::ResumeInvalid);
        }
        let mut file = File::from(descriptor);
        let (flags, last_timestamp_ms) = inspect_flv_tail(&mut file, length)?;
        if !claim.incarnation_active() {
            return Err(RecordingStoreError::CreationCancelled);
        }
        drop(claim);
        Ok(RecordingResume {
            file: RecordingFile {
                incarnation: self.handle.incarnation,
                shared,
                _ownership: ownership,
                file: Some(file),
                partial_name: relative_name.to_owned(),
                final_relative_name: relative_name.to_owned(),
                position: length,
                length,
                partial_exists: false,
                partial_exists_state: Arc::new(AtomicBool::new(false)),
                file_accounted: true,
                max_bytes,
                preserve_partial: Arc::new(AtomicBool::new(false)),
                commit: Arc::new(RecordingCommitState {
                    state: AtomicU8::new(CommitPhase::Open.encode()),
                    #[cfg(test)]
                    gate: Mutex::new(None),
                    #[cfg(test)]
                    fail_partial_unlink: AtomicBool::new(false),
                    #[cfg(test)]
                    fail_rollback_unlink: AtomicBool::new(false),
                    #[cfg(test)]
                    fail_rollback_sync: AtomicBool::new(false),
                }),
                resumed: true,
            },
            flags,
            last_timestamp_ms,
        })
    }

    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn quota_scope(&self) -> RecordingQuotaScope {
        RecordingQuotaScope::Process
    }

    pub(crate) fn retire(&self) {
        if let Some(shared) = self.handle.shared() {
            shared.retire_incarnation(self.handle.incarnation);
        }
        self.handle.retire();
    }

    fn shared(&self) -> Result<Arc<StoreShared>, RecordingStoreError> {
        self.handle
            .shared()
            .ok_or(RecordingStoreError::CreationCancelled)
    }

    fn from_shared(
        shared: Arc<StoreShared>,
        deadline: Option<Instant>,
    ) -> Result<Self, RecordingStoreError> {
        let incarnation = next_store_incarnation();
        shared.register_incarnation(incarnation, deadline)?;
        Ok(Self {
            handle: Arc::new(RecordingStoreHandle {
                incarnation,
                shared: Mutex::new(Some(shared)),
            }),
        })
    }

    #[cfg(test)]
    fn install_operation_gate(
        &self,
        kind: RecordingOperationGateKind,
    ) -> Arc<RecordingOperationGate> {
        self.shared()
            .expect("active recording store")
            .install_operation_gate(kind)
    }
}

/// Exclusive writable ownership of one recording path.
pub struct RecordingFile {
    incarnation: u64,
    shared: Arc<StoreShared>,
    _ownership: OwnedFd,
    file: Option<File>,
    partial_name: String,
    final_relative_name: String,
    position: u64,
    length: u64,
    partial_exists: bool,
    partial_exists_state: Arc<AtomicBool>,
    file_accounted: bool,
    max_bytes: Option<u64>,
    preserve_partial: Arc<AtomicBool>,
    commit: Arc<RecordingCommitState>,
    resumed: bool,
}

pub(crate) struct RecordingResume {
    pub file: RecordingFile,
    pub flags: u8,
    pub last_timestamp_ms: u32,
}

struct RecordingCommitState {
    state: AtomicU8,
    #[cfg(test)]
    gate: Mutex<Option<Arc<RecordingPublicationGate>>>,
    #[cfg(test)]
    fail_partial_unlink: AtomicBool,
    #[cfg(test)]
    fail_rollback_unlink: AtomicBool,
    #[cfg(test)]
    fail_rollback_sync: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitPhase {
    Open,
    Cancelled,
    Publishing,
    Finished,
}

impl CommitPhase {
    const fn encode(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Cancelled => 1,
            Self::Publishing => 2,
            Self::Finished => 3,
        }
    }

    fn decode(value: u8) -> Self {
        match value {
            0 => Self::Open,
            1 => Self::Cancelled,
            2 => Self::Publishing,
            3 => Self::Finished,
            _ => unreachable!("recording commit state is written only through CommitPhase"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RecordingCommitCancellation {
    state: Arc<RecordingCommitState>,
}

#[cfg(test)]
pub(crate) struct RecordingPublicationGate {
    state: Mutex<RecordingPublicationGateState>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
struct RecordingPublicationGateState {
    progress: RecordingPublicationGateProgress,
    claim_allowed: bool,
    publication_allowed: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum RecordingPublicationGateProgress {
    Installed,
    BeforeClaim,
    AfterClaim,
}

impl RecordingFile {
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.length
    }

    pub(crate) fn partial_relative_name(&self) -> &str {
        &self.partial_name
    }

    pub(crate) fn preservation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.preserve_partial)
    }

    pub(crate) fn partial_existence_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.partial_exists_state)
    }

    pub(crate) fn commit_cancellation(&self) -> RecordingCommitCancellation {
        RecordingCommitCancellation {
            state: Arc::clone(&self.commit),
        }
    }

    pub(crate) fn finalizer_authority(&self) -> RecordingFinalizerAuthority {
        RecordingFinalizerAuthority {
            incarnation: self.incarnation,
            shared: Arc::clone(&self.shared),
        }
    }

    pub(crate) fn set_final_relative_name(
        &mut self,
        final_relative_name: String,
    ) -> Result<(), RecordingStoreError> {
        validate_relative_name(&final_relative_name)?;
        self.final_relative_name = final_relative_name;
        Ok(())
    }

    #[cfg(test)]
    fn fail_publication_unlinks(&self) {
        self.commit
            .fail_partial_unlink
            .store(true, Ordering::Release);
        self.commit
            .fail_rollback_unlink
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn fail_partial_unlink(&self) {
        self.commit
            .fail_partial_unlink
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn fail_partial_unlink_and_rollback_sync(&self) {
        self.fail_partial_unlink();
        self.commit
            .fail_rollback_sync
            .store(true, Ordering::Release);
    }

    /// Flushes and synchronizes the recording, then publishes any renamed target without replacing
    /// an existing entry. A collision receives a bounded deterministic relative suffix.
    ///
    /// # Errors
    ///
    /// Returns an error when flushing, synchronizing, publishing, or synchronizing the containing
    /// directory fails. A directory-sync error carries the already-published recording.
    pub fn commit(mut self) -> Result<RecordingCommit, RecordingStoreError> {
        self.commit_inner()
    }

    fn commit_inner(&mut self) -> Result<RecordingCommit, RecordingStoreError> {
        self.preserve_partial.store(true, Ordering::Release);
        if self.commit.cancelled() {
            return Err(self.cancelled_error());
        }
        let operation = self
            .shared
            .claim(self.incarnation)
            .ok_or_else(|| self.cancelled_error())?;
        if let Some(file) = self.file.as_mut()
            && let Err(source) = file.flush().and_then(|()| file.sync_all())
        {
            self.commit.finish();
            return Err(RecordingStoreError::FileSync {
                partial_relative_name: self.partial_name.clone(),
                source,
            });
        }
        if self.commit.cancelled() {
            return Err(self.cancelled_error());
        }
        if !operation.incarnation_active() {
            return Err(self.cancelled_error());
        }
        if !self.partial_is_owned() {
            self.partial_exists = false;
            self.partial_exists_state.store(false, Ordering::Release);
            self.release_missing_file_accounting();
            self.commit.finish();
            return Err(RecordingStoreError::PartialOwnershipLost {
                partial_relative_name: self.partial_name.clone(),
            });
        }
        drop(operation);
        #[cfg(test)]
        self.commit.wait_before_publication();
        // Lock order: establish the store-state claim before changing the commit atomic. No path
        // may hold the commit gate/finalizer mutex while acquiring the store-state mutex.
        let _publication = self
            .shared
            .claim(self.incarnation)
            .ok_or_else(|| self.cancelled_error())?;
        if !self.commit.begin_publication() {
            return Err(self.cancelled_error());
        }
        #[cfg(test)]
        self.commit.wait_after_publication_claim();
        let relative_name = if self.partial_name == self.final_relative_name {
            self.final_relative_name.clone()
        } else {
            match self.publish() {
                Ok(relative_name) => relative_name,
                Err(error) => {
                    self.commit.finish();
                    return Err(error);
                }
            }
        };
        self.partial_exists = false;
        self.partial_exists_state.store(false, Ordering::Release);
        self.file_accounted = false;
        let recording = RecordingCommit {
            relative_name,
            bytes: self.length,
        };
        if let Err(source) = rustix_fs::fsync(&self.shared.root) {
            self.commit.finish();
            return Err(RecordingStoreError::PublishedDirectorySync {
                recording,
                source: source.into(),
            });
        }
        self.commit.finish();
        Ok(recording)
    }

    fn cancelled_error(&self) -> RecordingStoreError {
        RecordingStoreError::FinalizationCancelled {
            partial_relative_name: self.partial_name.clone(),
        }
    }

    fn partial_is_owned(&self) -> bool {
        let Some(file) = self.file.as_ref() else {
            return false;
        };
        let Ok(descriptor) = rustix_fs::fstat(file) else {
            return false;
        };
        let Ok(path) = rustix_fs::statat(
            &self.shared.root,
            self.partial_name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) else {
            return false;
        };
        FileType::from_raw_mode(path.st_mode).is_file()
            && descriptor.st_dev == path.st_dev
            && descriptor.st_ino == path.st_ino
    }

    fn publish(&mut self) -> Result<String, RecordingStoreError> {
        for attempt in 0..MAX_NAME_ATTEMPTS {
            let candidate = if attempt == 0 {
                self.final_relative_name.clone()
            } else {
                let Some(candidate) =
                    collision_recording_filename(&self.final_relative_name, attempt)
                else {
                    break;
                };
                candidate
            };
            let file = self
                .file
                .as_ref()
                .expect("recording descriptor remains open through publication");
            match rustix_fs::linkat(
                file,
                "",
                &self.shared.root,
                candidate.as_str(),
                AtFlags::EMPTY_PATH,
            ) {
                Ok(()) => {
                    if self.partial_is_owned() {
                        let unlink_partial = if publication_partial_unlink_fault(&self.commit) {
                            Err(Errno::IO)
                        } else {
                            rustix_fs::unlinkat(
                                &self.shared.root,
                                self.partial_name.as_str(),
                                AtFlags::empty(),
                            )
                        };
                        if let Err(source) = unlink_partial {
                            return Err(self.rollback_publication(candidate, source));
                        }
                    }
                    return Ok(candidate);
                }
                Err(Errno::EXIST) => {}
                Err(source)
                    if source == Errno::PERM
                        || source == Errno::INVAL
                        || source == Errno::NOTSUP =>
                {
                    return Err(RecordingStoreError::DescriptorPublicationUnsupported {
                        partial_relative_name: self.partial_name.clone(),
                    });
                }
                Err(source) => {
                    if !self.partial_is_owned() {
                        self.partial_exists = false;
                        self.partial_exists_state.store(false, Ordering::Release);
                        self.release_missing_file_accounting();
                        return Err(RecordingStoreError::PartialOwnershipLost {
                            partial_relative_name: self.partial_name.clone(),
                        });
                    }
                    return Err(RecordingStoreError::Publish {
                        partial_relative_name: self.partial_name.clone(),
                        source: source.into(),
                    });
                }
            }
        }
        Err(RecordingStoreError::FinalNameCollisions {
            partial_relative_name: self.partial_name.clone(),
        })
    }

    fn rollback_publication(&mut self, candidate: String, source: Errno) -> RecordingStoreError {
        let recording = RecordingCommit {
            relative_name: candidate,
            bytes: self.length,
        };
        let rollback = if publication_rollback_unlink_fault(&self.commit) {
            Err(Errno::IO)
        } else {
            rustix_fs::unlinkat(
                &self.shared.root,
                recording.relative_name.as_str(),
                AtFlags::empty(),
            )
        };
        if let Err(rollback_source) = rollback {
            self.account_duplicate_publication();
            return RecordingStoreError::PublishRollback {
                partial_relative_name: self.partial_name.clone(),
                recording,
                source: source.into(),
                rollback_source: rollback_source.into(),
            };
        }
        let rollback_sync = if publication_rollback_sync_fault(&self.commit) {
            Err(Errno::IO)
        } else {
            rustix_fs::fsync(&self.shared.root)
        };
        if let Err(directory_sync_source) = rollback_sync {
            return RecordingStoreError::PublishRollbackDirectorySync {
                partial_relative_name: self.partial_name.clone(),
                recording,
                source: source.into(),
                directory_sync_source: directory_sync_source.into(),
            };
        }
        RecordingStoreError::Publish {
            partial_relative_name: self.partial_name.clone(),
            source: source.into(),
        }
    }

    fn account_duplicate_publication(&mut self) {
        let mut state = self.shared.lock();
        state.files = state.files.saturating_add(1);
        state.bytes_used = state.bytes_used.saturating_add(self.length);
    }

    fn release_missing_file_accounting(&mut self) {
        if !self.file_accounted {
            return;
        }
        let mut state = self.shared.lock();
        state.files -= 1;
        state.bytes_used -= self.length;
        self.file_accounted = false;
    }
}

impl RecordingCommitState {
    fn cancelled(&self) -> bool {
        CommitPhase::decode(self.state.load(Ordering::Acquire)) == CommitPhase::Cancelled
    }

    fn begin_publication(&self) -> bool {
        self.state
            .compare_exchange(
                CommitPhase::Open.encode(),
                CommitPhase::Publishing.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn finish(&self) {
        if self
            .state
            .compare_exchange(
                CommitPhase::Publishing.encode(),
                CommitPhase::Finished.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            let _ = self.state.compare_exchange(
                CommitPhase::Open.encode(),
                CommitPhase::Finished.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    #[cfg(test)]
    fn wait_before_publication(&self) {
        let gate = self
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned")
            .clone();
        let Some(gate) = gate else {
            return;
        };
        let mut state = gate
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        state.progress = RecordingPublicationGateProgress::BeforeClaim;
        gate.changed.notify_all();
        while !state.claim_allowed && !self.cancelled() {
            state = gate
                .changed
                .wait(state)
                .expect("recording publication gate state mutex poisoned");
        }
    }

    #[cfg(test)]
    fn wait_after_publication_claim(&self) {
        let gate = self
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned")
            .clone();
        let Some(gate) = gate else {
            return;
        };
        let mut state = gate
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        state.progress = RecordingPublicationGateProgress::AfterClaim;
        gate.changed.notify_all();
        while !state.publication_allowed {
            state = gate
                .changed
                .wait(state)
                .expect("recording publication gate state mutex poisoned");
        }
    }
}

#[cfg(test)]
fn publication_partial_unlink_fault(commit: &RecordingCommitState) -> bool {
    commit.fail_partial_unlink.swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
const fn publication_partial_unlink_fault(_: &RecordingCommitState) -> bool {
    false
}

#[cfg(test)]
fn publication_rollback_unlink_fault(commit: &RecordingCommitState) -> bool {
    commit.fail_rollback_unlink.swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
const fn publication_rollback_unlink_fault(_: &RecordingCommitState) -> bool {
    false
}

#[cfg(test)]
fn publication_rollback_sync_fault(commit: &RecordingCommitState) -> bool {
    commit.fail_rollback_sync.swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
const fn publication_rollback_sync_fault(_: &RecordingCommitState) -> bool {
    false
}

impl RecordingCommitCancellation {
    pub(crate) fn cancel(&self) -> bool {
        let cancelled = self
            .state
            .state
            .compare_exchange(
                CommitPhase::Open.encode(),
                CommitPhase::Cancelled.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        #[cfg(test)]
        if let Some(gate) = self
            .state
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned")
            .as_ref()
        {
            gate.changed.notify_all();
        }
        cancelled
    }

    pub(crate) fn finish(&self) {
        self.state.finish();
    }

    #[cfg(test)]
    pub(crate) fn install_publication_gate(&self) -> Arc<RecordingPublicationGate> {
        let gate = Arc::new(RecordingPublicationGate {
            state: Mutex::new(RecordingPublicationGateState {
                progress: RecordingPublicationGateProgress::Installed,
                claim_allowed: false,
                publication_allowed: false,
            }),
            changed: std::sync::Condvar::new(),
        });
        *self
            .state
            .gate
            .lock()
            .expect("recording publication gate mutex poisoned") = Some(Arc::clone(&gate));
        gate
    }
}

#[cfg(test)]
impl RecordingPublicationGate {
    pub(crate) fn wait_before_claim(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        self.changed
            .wait_timeout_while(state, timeout, |state| {
                state.progress == RecordingPublicationGateProgress::Installed
            })
            .expect("recording publication gate state mutex poisoned")
            .0
            .progress
            != RecordingPublicationGateProgress::Installed
    }

    pub(crate) fn allow_claim(&self) {
        self.state
            .lock()
            .expect("recording publication gate state mutex poisoned")
            .claim_allowed = true;
        self.changed.notify_all();
    }

    pub(crate) fn wait_after_claim(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .expect("recording publication gate state mutex poisoned");
        self.changed
            .wait_timeout_while(state, timeout, |state| {
                state.progress != RecordingPublicationGateProgress::AfterClaim
            })
            .expect("recording publication gate state mutex poisoned")
            .0
            .progress
            == RecordingPublicationGateProgress::AfterClaim
    }

    pub(crate) fn allow_publication(&self) {
        self.state
            .lock()
            .expect("recording publication gate state mutex poisoned")
            .publication_allowed = true;
        self.changed.notify_all();
    }
}

impl Write for RecordingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let _claim = self.shared.claim(self.incarnation).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "recording store incarnation retired",
            )
        })?;
        let requested = u64::try_from(buffer.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "recording write is too large")
        })?;
        let requested_end = self.position.checked_add(requested).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "recording position overflow")
        })?;
        if self
            .max_bytes
            .is_some_and(|maximum| requested_end > maximum)
        {
            return Err(byte_quota_error(
                self.max_bytes.expect("recording byte limit was checked"),
            ));
        }
        let reserved_growth = requested_end.saturating_sub(self.length);
        {
            let mut state = self.shared.lock();
            let Some(new_total) = state.bytes_used.checked_add(reserved_growth) else {
                return Err(byte_quota_error(
                    self.shared.limits.max_bytes.unwrap_or(u64::MAX),
                ));
            };
            if let Some(maximum) = self.shared.limits.max_bytes
                && new_total > maximum
            {
                return Err(byte_quota_error(maximum));
            }
            state.bytes_used = new_total;
        }

        let result = self
            .file
            .as_mut()
            .expect("recording file is writable until commit")
            .write(buffer);
        match result {
            Ok(written) => {
                let written = u64::try_from(written).expect("written byte count fits in u64");
                let actual_end = self.position + written;
                let actual_growth = actual_end.saturating_sub(self.length);
                if actual_growth < reserved_growth {
                    self.shared.lock().bytes_used -= reserved_growth - actual_growth;
                }
                self.position = actual_end;
                self.length = self.length.max(actual_end);
                Ok(usize::try_from(written).expect("write returned a usize byte count"))
            }
            Err(source) => {
                self.shared.lock().bytes_used -= reserved_growth;
                Err(source)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let _claim = self.shared.claim(self.incarnation).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "recording store incarnation retired",
            )
        })?;
        self.file
            .as_mut()
            .expect("recording file is writable until commit")
            .flush()
    }
}

impl Seek for RecordingFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let _claim = self.shared.claim(self.incarnation).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "recording store incarnation retired",
            )
        })?;
        let position = self
            .file
            .as_mut()
            .expect("recording file is seekable until commit")
            .seek(position)?;
        self.position = position;
        Ok(position)
    }
}

impl Drop for RecordingFile {
    fn drop(&mut self) {
        if !self.file_accounted && !self.partial_exists {
            return;
        }
        let Some(_claim) = self.shared.claim(self.incarnation) else {
            return;
        };
        if self.resumed {
            let owned = self.partial_is_owned();
            drop(self.file.take());
            if !owned {
                self.partial_exists_state.store(false, Ordering::Release);
                self.release_missing_file_accounting();
            }
            return;
        }

        let owned = self.partial_is_owned();
        drop(self.file.take());
        let preserve_partial = self.preserve_partial.load(Ordering::Acquire);
        let removed = if self.partial_exists && !preserve_partial && owned {
            match rustix_fs::unlinkat(
                &self.shared.root,
                self.partial_name.as_str(),
                AtFlags::empty(),
            ) {
                Ok(()) | Err(Errno::NOENT) => true,
                Err(_) => false,
            }
        } else {
            !(self.partial_exists && preserve_partial && owned)
        };
        let mut state = self.shared.lock();
        if removed && self.file_accounted {
            state.files -= 1;
            state.bytes_used -= self.length;
            self.file_accounted = false;
        }
        if removed || !owned {
            self.partial_exists_state.store(false, Ordering::Release);
        }
    }
}

impl Drop for RecorderLease {
    fn drop(&mut self) {
        self.shared.lock().active_recorders -= 1;
    }
}

#[derive(Clone)]
pub(crate) struct RecordingFinalizerAuthority {
    incarnation: u64,
    shared: Arc<StoreShared>,
}

impl RecordingFinalizerAuthority {
    pub(crate) fn submit(&self, job: FinalizerJob) -> FinalizerTicket {
        let shared = Arc::clone(&self.shared);
        let incarnation = self.incarnation;
        self.shared.finalizer.submit(Box::new(move || {
            // The job still runs after retirement so its file can become recoverable. Publication
            // remains fenced by the same incarnation inside RecordingFile::commit.
            let _retired = !shared.incarnation_active(incarnation);
            job();
        }))
    }
}

impl RecordingFinalizer {
    fn new(max_active_recorders: usize) -> Result<Self, RecordingStoreError> {
        let queue_capacity = max_active_recorders
            .saturating_mul(MAX_PENDING_FINALIZATIONS_PER_RECORDER)
            .max(1);
        let thread_count = queue_capacity.min(MAX_FINALIZER_THREADS);
        let shared = Arc::new(FinalizerShared {
            state: Mutex::new(FinalizerState::default()),
            available: Condvar::new(),
            space_available: Condvar::new(),
            queue_capacity,
        });
        let mut threads = Vec::with_capacity(thread_count);
        for index in 0..thread_count {
            let worker_shared = Arc::clone(&shared);
            match thread::Builder::new()
                .name(format!("rtmp-recording-finalizer-{index}"))
                .spawn(move || run_finalizer(&worker_shared))
            {
                Ok(thread) => threads.push(thread),
                Err(source) => {
                    shared.lock().stopping = true;
                    shared.available.notify_all();
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(RecordingStoreError::FinalizerThreadSpawn(source));
                }
            }
        }
        Ok(Self { shared, threads })
    }

    fn submit(&self, job: FinalizerJob) -> FinalizerTicket {
        let mut state = self.shared.lock();
        while state.jobs.len() >= self.shared.queue_capacity && !state.stopping {
            state = self
                .shared
                .space_available
                .wait(state)
                .expect("recording finalizer mutex poisoned while waiting for capacity");
        }
        assert!(
            !state.stopping,
            "recording finalizer stopped before its store"
        );
        let id = state.next_job_id;
        state.next_job_id = state
            .next_job_id
            .checked_add(1)
            .expect("recording finalizer job identity exhausted");
        state.jobs.push_back(FinalizerJobEntry { id, job });
        drop(state);
        self.shared.available.notify_one();
        FinalizerTicket {
            shared: Arc::downgrade(&self.shared),
            id,
        }
    }
}

impl FinalizerTicket {
    pub(crate) fn cancel_queued(&self) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let mut state = shared.lock();
        let Some(position) = state.jobs.iter().position(|job| job.id == self.id) else {
            return false;
        };
        let job = state
            .jobs
            .remove(position)
            .expect("queued finalization position remains valid");
        drop(state);
        shared.space_available.notify_one();
        drop(job);
        true
    }
}

impl Drop for RecordingFinalizer {
    fn drop(&mut self) {
        self.shared.lock().stopping = true;
        self.shared.available.notify_all();
        self.shared.space_available.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl FinalizerShared {
    fn lock(&self) -> MutexGuard<'_, FinalizerState> {
        self.state
            .lock()
            .expect("recording finalizer mutex poisoned")
    }
}

fn run_finalizer(shared: &FinalizerShared) {
    loop {
        let mut state = shared.lock();
        while state.jobs.is_empty() && !state.stopping {
            state = shared
                .available
                .wait(state)
                .expect("recording finalizer mutex poisoned while waiting");
        }
        let Some(job) = state.jobs.pop_front() else {
            return;
        };
        drop(state);
        shared.space_available.notify_one();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.job));
    }
}

impl StoreShared {
    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().expect("recording store mutex poisoned")
    }

    #[cfg(test)]
    fn install_operation_gate(
        &self,
        kind: RecordingOperationGateKind,
    ) -> Arc<RecordingOperationGate> {
        let gate = Arc::new(RecordingOperationGate {
            state: Mutex::new(RecordingOperationGateState {
                kind,
                reached: false,
                released: false,
            }),
            changed: Condvar::new(),
        });
        *self
            .operation_gate
            .lock()
            .expect("recording operation gate mutex poisoned") = Some(Arc::clone(&gate));
        gate
    }

    fn register_incarnation(
        &self,
        incarnation: u64,
        deadline: Option<Instant>,
    ) -> Result<(), RecordingStoreError> {
        let mut state = self.lock();
        while state
            .incarnations
            .values()
            .any(|incarnation| !incarnation.active && incarnation.claims != 0)
        {
            let Some(deadline) = deadline else {
                state = self
                    .incarnation_changed
                    .wait(state)
                    .expect("recording store mutex poisoned while waiting for retirement fencing");
                continue;
            };
            let now = Instant::now();
            if now >= deadline {
                return Err(RecordingStoreError::CreationCancelled);
            }
            #[cfg(test)]
            {
                drop(state);
                self.wait_at_operation_gate(RecordingOperationGateKind::RegisterBeforeWait);
                state = self.lock();
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RecordingStoreError::CreationCancelled);
            }
            let (next, wait) = self
                .incarnation_changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("recording store mutex poisoned while waiting for retirement fencing");
            state = next;
            #[cfg(test)]
            {
                drop(state);
                self.wait_at_operation_gate(RecordingOperationGateKind::RegisterAfterWake);
                state = self.lock();
            }
            if Instant::now() >= deadline {
                return Err(RecordingStoreError::CreationCancelled);
            }
            if wait.timed_out() {
                return Err(RecordingStoreError::CreationCancelled);
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RecordingStoreError::CreationCancelled);
        }
        state
            .incarnations
            .retain(|_, incarnation| incarnation.active || incarnation.claims != 0);
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RecordingStoreError::CreationCancelled);
        }
        state.incarnations.insert(
            incarnation,
            IncarnationState {
                active: true,
                claims: 0,
            },
        );
        Ok(())
    }

    #[cfg(test)]
    fn incarnation_count(&self) -> usize {
        self.lock().incarnations.len()
    }

    fn retire_incarnation(&self, incarnation: u64) {
        let mut state = self.lock();
        if let Some(incarnation) = state.incarnations.get_mut(&incarnation) {
            incarnation.active = false;
        }
        state
            .incarnations
            .retain(|_, incarnation| incarnation.active || incarnation.claims != 0);
        drop(state);
        self.incarnation_changed.notify_all();
    }

    fn incarnation_active(&self, incarnation: u64) -> bool {
        self.lock()
            .incarnations
            .get(&incarnation)
            .is_some_and(|incarnation| incarnation.active)
    }

    fn claim(self: &Arc<Self>, incarnation: u64) -> Option<StoreOperationClaim> {
        let mut state = self.lock();
        let incarnation_state = state.incarnations.get_mut(&incarnation)?;
        if !incarnation_state.active {
            return None;
        }
        incarnation_state.claims = incarnation_state
            .claims
            .checked_add(1)
            .expect("recording store operation claim count exhausted");
        drop(state);
        Some(StoreOperationClaim {
            incarnation,
            shared: Arc::clone(self),
        })
    }

    #[cfg(test)]
    fn wait_at_operation_gate(&self, kind: RecordingOperationGateKind) {
        let gate = self
            .operation_gate
            .lock()
            .expect("recording operation gate mutex poisoned")
            .clone();
        let Some(gate) = gate else {
            return;
        };
        let mut state = gate
            .state
            .lock()
            .expect("recording operation gate state mutex poisoned");
        if state.kind != kind {
            return;
        }
        state.reached = true;
        gate.changed.notify_all();
        while !state.released {
            state = gate
                .changed
                .wait(state)
                .expect("recording operation gate state mutex poisoned");
        }
    }
}

#[cfg(test)]
impl RecordingOperationGate {
    fn wait_reached(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .expect("recording operation gate state mutex poisoned");
        self.changed
            .wait_timeout_while(state, timeout, |state| !state.reached)
            .expect("recording operation gate state mutex poisoned")
            .0
            .reached
    }

    fn release(&self) {
        self.state
            .lock()
            .expect("recording operation gate state mutex poisoned")
            .released = true;
        self.changed.notify_all();
    }
}

impl StoreOperationClaim {
    fn incarnation_active(&self) -> bool {
        self.shared.incarnation_active(self.incarnation)
    }
}

impl Drop for StoreOperationClaim {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        let incarnation = state
            .incarnations
            .get_mut(&self.incarnation)
            .expect("claimed recording store incarnation remains registered");
        incarnation.claims -= 1;
        let remove = !incarnation.active && incarnation.claims == 0;
        if remove {
            state.incarnations.remove(&self.incarnation);
        }
        drop(state);
        self.shared.incarnation_changed.notify_all();
    }
}

impl RecordingStoreHandle {
    fn shared(&self) -> Option<Arc<StoreShared>> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn retire(&self) {
        let shared = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(shared);
    }
}

fn scan_root(
    root: &OwnedFd,
    limits: RecordingStoreLimits,
    clean_partials: bool,
) -> Result<StoreState, RecordingStoreError> {
    let mut directory =
        Dir::read_from(root).map_err(|source| RecordingStoreError::RootRead(source.into()))?;
    let mut state = StoreState::default();
    let mut cleaned = false;

    for entry in &mut directory {
        let entry = entry.map_err(|source| RecordingStoreError::RootRead(source.into()))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..")
            || name.to_bytes() == OWNERSHIP_LOCK_NAME.as_bytes()
            || is_owner_probe(name.to_bytes())
        {
            continue;
        }
        let metadata = match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(Errno::NOENT) => continue,
            Err(source) => return Err(RecordingStoreError::RootEntryMetadata(source.into())),
        };
        let is_regular = FileType::from_raw_mode(metadata.st_mode).is_file();
        if clean_partials && is_regular && is_owned_partial(name.to_bytes()) {
            match rustix_fs::unlinkat(root, name, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {
                    cleaned = true;
                    continue;
                }
                Err(source) => return Err(RecordingStoreError::PartialCleanup(source.into())),
            }
        }
        if !is_regular {
            continue;
        }

        state.files =
            state
                .files
                .checked_add(1)
                .ok_or(RecordingStoreError::ExistingUsageExceedsLimits {
                    bytes_used: u64::MAX,
                    files: usize::MAX,
                })?;
        let size = u64::try_from(metadata.st_size).unwrap_or(u64::MAX);
        state.bytes_used = state.bytes_used.saturating_add(size);
    }

    if cleaned {
        rustix_fs::fsync(root).map_err(|source| RecordingStoreError::RootSync(source.into()))?;
    }
    if limits
        .max_bytes
        .is_some_and(|maximum| state.bytes_used > maximum)
        || limits
            .max_files
            .is_some_and(|maximum| state.files > maximum)
    {
        return Err(RecordingStoreError::ExistingUsageExceedsLimits {
            bytes_used: state.bytes_used,
            files: state.files,
        });
    }
    Ok(state)
}

fn validate_relative_name(name: &str) -> Result<(), RecordingStoreError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > MAX_RECORDING_FILENAME_BYTES
        || name.as_bytes().contains(&0)
        || name.as_bytes().contains(&b'/')
        || name == OWNERSHIP_LOCK_NAME
        || is_owned_partial(name.as_bytes())
        || is_owner_probe(name.as_bytes())
    {
        return Err(RecordingStoreError::InvalidRelativeName);
    }
    Ok(())
}

fn create_recording_entry(
    shared: &StoreShared,
    claim: &StoreOperationClaim,
    final_relative_name: &str,
    lock: bool,
) -> Result<(OwnedFd, String), RecordingStoreError> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let Some(relative_name) = (if attempt == 0 {
            Some(final_relative_name.to_owned())
        } else {
            collision_recording_filename(final_relative_name, attempt)
        }) else {
            break;
        };
        match rustix_fs::openat(
            &shared.root,
            relative_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(descriptor) => {
                if lock
                    && let Err(source) =
                        rustix_fs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive)
                {
                    drop(descriptor);
                    unlink_created_entry(shared, &relative_name)?;
                    return Err(RecordingStoreError::PartialCreate(source.into()));
                }
                if !claim.incarnation_active() {
                    drop(descriptor);
                    unlink_created_entry(shared, &relative_name)?;
                    return Err(RecordingStoreError::CreationCancelled);
                }
                return Ok((descriptor, relative_name));
            }
            Err(Errno::EXIST) => {}
            Err(source) => return Err(RecordingStoreError::PartialCreate(source.into())),
        }
    }
    Err(RecordingStoreError::FinalNameCollisions {
        partial_relative_name: final_relative_name.to_owned(),
    })
}

fn unlink_created_entry(
    shared: &StoreShared,
    relative_name: &str,
) -> Result<(), RecordingStoreError> {
    match rustix_fs::unlinkat(&shared.root, relative_name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(cleanup) => Err(RecordingStoreError::PartialCleanup(cleanup.into())),
    }
}

fn is_owned_partial(name: &[u8]) -> bool {
    let Some(token) = name
        .strip_prefix(PARTIAL_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(PARTIAL_SUFFIX.as_bytes()))
    else {
        return false;
    };
    token.len() == UUID_SIMPLE_LENGTH
        && token
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_owner_probe(name: &[u8]) -> bool {
    let Some(token) = name.strip_prefix(OWNER_PROBE_PREFIX.as_bytes()) else {
        return false;
    };
    token.len() == UUID_SIMPLE_LENGTH
        && token
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn open_pinned_directory(path: &Path) -> Result<OwnedFd, Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory = rustix_fs::open(
        if path.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        },
        flags,
        Mode::empty(),
    )?;

    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = rustix_fs::openat(&directory, name, flags, Mode::empty())?;
            }
            Component::ParentDir | Component::Prefix(_) => return Err(Errno::INVAL),
        }
    }
    Ok(directory)
}

fn verify_root_control(root: &OwnedFd) -> Result<rustix_fs::Stat, RecordingStoreError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| RecordingStoreError::RootNotExclusive)?;
    if metadata.st_mode & 0o022 != 0 {
        return Err(RecordingStoreError::RootNotExclusive);
    }

    for _ in 0..MAX_NAME_ATTEMPTS {
        let probe_name = format!("{OWNER_PROBE_PREFIX}{}", Uuid::new_v4().simple());
        match rustix_fs::openat(
            root,
            probe_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(probe) => {
                let probe_metadata =
                    rustix_fs::fstat(&probe).map_err(|_| RecordingStoreError::RootNotExclusive)?;
                rustix_fs::unlinkat(root, probe_name.as_str(), AtFlags::empty())
                    .map_err(|_| RecordingStoreError::RootNotExclusive)?;
                if metadata.st_uid != probe_metadata.st_uid {
                    return Err(RecordingStoreError::RootNotExclusive);
                }
                return Ok(metadata);
            }
            Err(Errno::EXIST) => {}
            Err(_) => return Err(RecordingStoreError::RootNotExclusive),
        }
    }
    Err(RecordingStoreError::RootNotExclusive)
}

fn verify_root_control_read_only(root: &OwnedFd) -> Result<(), RecordingStoreError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| RecordingStoreError::RootNotExclusive)?;
    let process =
        rustix_fs::stat("/proc/self").map_err(|_| RecordingStoreError::RootNotExclusive)?;
    if metadata.st_uid != process.st_uid || metadata.st_mode & 0o022 != 0 {
        return Err(RecordingStoreError::RootNotExclusive);
    }
    Ok(())
}

fn acquire_shared_ownership(
    root: &OwnedFd,
    cancelled: &impl Fn() -> bool,
) -> Result<OwnedFd, RecordingStoreError> {
    let ownership = rustix_fs::openat(
        root,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|source| RecordingStoreError::OwnershipOpen(source.into()))?;
    loop {
        if cancelled() {
            return Err(RecordingStoreError::CreationCancelled);
        }
        match rustix_fs::flock(&ownership, FlockOperation::NonBlockingLockShared) {
            Ok(()) => return Ok(ownership),
            Err(source) if source == Errno::AGAIN || source == Errno::WOULDBLOCK => {
                thread::sleep(OWNERSHIP_RETRY_INTERVAL);
            }
            Err(source) => return Err(RecordingStoreError::OwnershipLock(source.into())),
        }
    }
}

fn store_registry() -> &'static Mutex<HashMap<RootIdentity, Weak<StoreShared>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<RootIdentity, Weak<StoreShared>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_store_incarnation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}

fn byte_quota_error(maximum: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        format!("recording byte limit of {maximum} reached"),
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::mpsc};

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn finalizer_runs_one_root_job_at_a_time() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024),
                max_files: Some(8),
                max_active_recorders: 4,
            },
        )
        .expect("recording store");
        assert_eq!(
            store
                .shared()
                .expect("active recording store")
                .finalizer
                .threads
                .len(),
            MAX_FINALIZER_THREADS
        );
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        let authority = RecordingFinalizerAuthority {
            incarnation: store.handle.incarnation,
            shared: store.shared().expect("active recording store"),
        };

        for job in 0..4 {
            let release = Arc::clone(&release);
            let started_tx = started_tx.clone();
            let completed_tx = completed_tx.clone();
            authority.submit(Box::new(move || {
                started_tx.send(job).expect("report started finalization");
                let (lock, available) = &*release;
                drop(
                    available
                        .wait_while(lock.lock().expect("release mutex poisoned"), |released| {
                            !*released
                        })
                        .expect("release mutex poisoned while waiting"),
                );
                completed_tx
                    .send(job)
                    .expect("report completed finalization");
            }));
        }

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first finalizer worker");
        assert!(
            started_rx.recv_timeout(Duration::from_millis(40)).is_err(),
            "more than one root finalization ran concurrently"
        );

        *release.0.lock().expect("release mutex poisoned") = true;
        release.1.notify_all();
        for _ in 0..3 {
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("queued finalization started");
        }
        for _ in 0..4 {
            completed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("finalization completed");
        }
    }

    #[test]
    fn bounded_reopen_registration_expires_without_registering_a_new_incarnation() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let gate = store.install_operation_gate(RecordingOperationGateKind::CreateBeforeOpen);
        let creating_store = store.clone();
        let creator = thread::spawn(move || creating_store.create("blocked.flv"));
        assert!(gate.wait_reached(Duration::from_secs(1)));
        let shared = store.shared().expect("shared store");
        store.retire();

        let started = Instant::now();
        let result = RecordingStore::open_with_deadline(
            root.path(),
            test_limits(),
            Some(started + Duration::from_millis(50)),
        );

        assert!(matches!(
            result,
            Err(RecordingStoreError::CreationCancelled)
        ));
        assert!(started.elapsed() >= Duration::from_millis(35));
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(shared.incarnation_count(), 1);
        gate.release();
        assert!(matches!(
            creator.join().expect("creator"),
            Err(RecordingStoreError::CreationCancelled)
        ));
        assert_eq!(shared.incarnation_count(), 0);
    }

    #[test]
    fn bounded_reopen_rejects_a_late_claim_release_without_inserting() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let shared = store.shared().expect("shared store");
        let claim = shared
            .claim(store.handle.incarnation)
            .expect("inactive claim fixture");
        let before_wait_gate =
            store.install_operation_gate(RecordingOperationGateKind::RegisterBeforeWait);
        store.retire();

        let root_path = root.path().to_owned();
        let (opened_tx, opened_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + Duration::from_millis(100);
        let opener = thread::spawn(move || {
            opened_tx
                .send(RecordingStore::open_with_deadline(
                    root_path,
                    test_limits(),
                    Some(deadline),
                ))
                .expect("bounded opener result receiver");
        });

        assert!(before_wait_gate.wait_reached(Duration::from_secs(1)));
        let registration_gate =
            shared.install_operation_gate(RecordingOperationGateKind::RegisterAfterWake);
        before_wait_gate.release();
        drop(claim);
        assert!(registration_gate.wait_reached(Duration::from_secs(1)));
        thread::sleep(Duration::from_millis(150));
        registration_gate.release();

        assert!(matches!(
            opened_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("late bounded opener"),
            Err(RecordingStoreError::CreationCancelled)
        ));
        opener.join().expect("opener");
        assert_eq!(shared.incarnation_count(), 0);

        let reopened = test_store(root.path());
        reopened
            .create("fresh.flv")
            .expect("reopen after late wake");
    }

    #[test]
    fn retired_incarnation_preserves_late_finalization_as_recoverable_partial() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let mut recording = store.create("staging.flv").expect("recording");
        recording.write_all(b"recoverable").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
        recording
            .set_final_relative_name("camera.flv".to_owned())
            .expect("final name");
        let authority = recording.finalizer_authority();
        let (result_tx, result_rx) = mpsc::sync_channel(1);

        store.retire();
        let reopened = test_store(root.path());
        let ticket = authority.submit(Box::new(move || {
            result_tx
                .send(recording.commit())
                .expect("late finalization result");
        }));

        let error = result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("late finalization")
            .expect_err("retired incarnation cannot publish");
        assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
        assert!(root.path().join(&partial).is_file());
        assert!(!root.path().join("camera.flv").exists());
        assert_eq!(reopened.stats().bytes_used, 11);
        assert_eq!(reopened.stats().files, 1);
        assert!(!ticket.cancel_queued());
        let fresh = reopened
            .create("camera.flv")
            .expect("fresh generation recording");
        drop(fresh);
        assert!(root.path().join(&partial).is_file());
    }

    #[test]
    fn retirement_before_publication_claim_reopens_with_only_the_recoverable_partial() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let mut recording = store.create("staging.flv").expect("recording");
        recording.write_all(b"recoverable").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
        recording
            .set_final_relative_name("camera.flv".to_owned())
            .expect("final name");
        let gate = recording.commit_cancellation().install_publication_gate();
        let (committed_tx, committed_rx) = mpsc::sync_channel(1);
        let committer = thread::spawn(move || {
            committed_tx
                .send(recording.commit())
                .expect("commit result receiver");
        });
        assert!(gate.wait_before_claim(Duration::from_secs(1)));

        store.retire();
        let reopened = test_store(root.path());
        gate.allow_claim();

        let error = committed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("retired commit result")
            .expect_err("retirement wins before publication claim");
        assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
        assert!(root.path().join(&partial).is_file());
        assert!(!root.path().join("camera.flv").exists());
        assert_eq!(reopened.stats().bytes_used, 11);
        assert_eq!(reopened.stats().files, 1);
        committer.join().expect("committer");
    }

    #[test]
    fn retirement_fences_create_before_open_and_reopen_waits_for_the_claim() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let gate = store.install_operation_gate(RecordingOperationGateKind::CreateBeforeOpen);
        let creating_store = store.clone();
        let (created_tx, created_rx) = mpsc::sync_channel(1);
        let creator = thread::spawn(move || {
            created_tx
                .send(creating_store.create("old.flv"))
                .expect("create result receiver");
        });
        assert!(gate.wait_reached(Duration::from_secs(1)));

        store.retire();
        let root_path = root.path().to_owned();
        let (reopened_tx, reopened_rx) = mpsc::sync_channel(1);
        let opener = thread::spawn(move || {
            reopened_tx
                .send(test_store(&root_path))
                .expect("reopened store receiver");
        });
        assert!(matches!(
            reopened_rx.recv_timeout(Duration::from_millis(40)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        gate.release();
        assert!(matches!(
            created_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("retired create result"),
            Err(RecordingStoreError::CreationCancelled)
        ));
        let reopened = reopened_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reopen after create claim");
        assert_eq!(reopened.stats(), RecordingStoreStats::default());
        assert!(!root.path().join("old.flv").exists());
        reopened.create("fresh.flv").expect("fresh create");
        creator.join().expect("creator");
        opener.join().expect("opener");
    }

    #[test]
    fn retirement_fences_resume_after_shared_capture() {
        let root = tempdir().expect("recording root");
        let mut flv = Vec::from(&b"FLV\x01\x04\0\0\0\x09\0\0\0\0"[..]);
        flv.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        flv.extend_from_slice(&11_u32.to_be_bytes());
        fs::write(root.path().join("camera.flv"), &flv).expect("existing FLV");
        let store = test_store(root.path());
        let gate =
            store.install_operation_gate(RecordingOperationGateKind::ResumeAfterSharedCapture);
        let resuming_store = store.clone();
        let (resumed_tx, resumed_rx) = mpsc::sync_channel(1);
        let resumer = thread::spawn(move || {
            resumed_tx
                .send(resuming_store.resume_with_options("camera.flv", false, None))
                .expect("resume result receiver");
        });
        assert!(gate.wait_reached(Duration::from_secs(1)));

        store.retire();
        let reopened = test_store(root.path());
        gate.release();

        assert!(matches!(
            resumed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("retired resume result"),
            Err(RecordingStoreError::CreationCancelled)
        ));
        assert_eq!(
            reopened.stats().bytes_used,
            u64::try_from(flv.len()).unwrap()
        );
        assert_eq!(reopened.stats().files, 1);
        assert_eq!(fs::read(root.path().join("camera.flv")).unwrap(), flv);
        resumer.join().expect("resumer");
    }

    #[test]
    fn claimed_publication_finishes_before_reopen_without_overwrite() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let mut recording = store.create("staging.flv").expect("recording");
        recording.write_all(b"old").expect("old data");
        recording
            .set_final_relative_name("camera.flv".to_owned())
            .expect("final name");
        let cancellation = recording.commit_cancellation();
        let gate = cancellation.install_publication_gate();
        let (committed_tx, committed_rx) = mpsc::sync_channel(1);
        let committer = thread::spawn(move || {
            committed_tx
                .send(recording.commit())
                .expect("commit result receiver");
        });
        assert!(gate.wait_before_claim(Duration::from_secs(1)));
        gate.allow_claim();
        assert!(gate.wait_after_claim(Duration::from_secs(1)));

        store.retire();
        let root_path = root.path().to_owned();
        let (reopened_tx, reopened_rx) = mpsc::sync_channel(1);
        let opener = thread::spawn(move || {
            reopened_tx
                .send(test_store(&root_path))
                .expect("reopened store receiver");
        });
        assert!(matches!(
            reopened_rx.recv_timeout(Duration::from_millis(40)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        gate.allow_publication();
        let commit = committed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("claimed publication")
            .expect("old claim publishes before reopen");
        assert_eq!(commit.relative_name, "camera.flv");
        let reopened = reopened_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reopen after publication claim");
        let mut fresh = reopened
            .create("camera.flv")
            .expect("fresh collision create");
        fresh.write_all(b"fresh").expect("fresh data");
        let fresh = fresh.commit().expect("fresh commit");
        assert_eq!(fresh.relative_name, "camera-1.flv");
        assert_eq!(fs::read(root.path().join("camera.flv")).unwrap(), b"old");
        assert_eq!(
            fs::read(root.path().join("camera-1.flv")).unwrap(),
            b"fresh"
        );
        assert_eq!(reopened.stats().bytes_used, 8);
        assert_eq!(reopened.stats().files, 2);
        committer.join().expect("committer");
        opener.join().expect("opener");
    }

    #[test]
    fn blocked_startup_scan_does_not_block_an_unrelated_root() {
        let blocked_root = tempdir().expect("blocked recording root");
        let unrelated_root = tempdir().expect("unrelated recording root");
        let ownership = fs::File::open(blocked_root.path()).expect("blocked root ownership");
        rustix_fs::flock(&ownership, FlockOperation::LockExclusive)
            .expect("stall blocked root scan");
        let blocked_path = blocked_root.path().to_owned();
        let (blocked_tx, blocked_rx) = mpsc::channel();
        let blocked_opener = thread::spawn(move || {
            let store = RecordingStore::open(blocked_path, test_limits()).expect("blocked store");
            blocked_tx
                .send(store.stats())
                .expect("report blocked store");
        });
        assert!(matches!(
            blocked_rx.recv_timeout(Duration::from_millis(40)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let unrelated_path = unrelated_root.path().to_owned();
        let (unrelated_tx, unrelated_rx) = mpsc::channel();
        let unrelated_opener = thread::spawn(move || {
            let store =
                RecordingStore::open(unrelated_path, test_limits()).expect("unrelated store");
            unrelated_tx
                .send(store.stats())
                .expect("report unrelated store");
        });
        let unrelated_result = unrelated_rx.recv_timeout(Duration::from_secs(1));

        rustix_fs::flock(&ownership, FlockOperation::Unlock).expect("release blocked root scan");
        assert_eq!(
            blocked_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("blocked store opens after release"),
            RecordingStoreStats::default()
        );
        blocked_opener.join().expect("blocked store opener");
        unrelated_opener.join().expect("unrelated store opener");
        assert_eq!(
            unrelated_result.expect("unrelated root opens without waiting"),
            RecordingStoreStats::default()
        );
    }

    #[test]
    fn resumed_file_drop_releases_accounting_when_ownership_is_lost() {
        let root = tempdir().expect("recording root");
        let mut flv = Vec::from(&b"FLV\x01\x04\0\0\0\x09\0\0\0\0"[..]);
        flv.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        flv.extend_from_slice(&11_u32.to_be_bytes());
        fs::write(root.path().join("camera.flv"), &flv).expect("existing FLV");
        let store = test_store(root.path());
        let resumed = store
            .resume_with_options("camera.flv", false, None)
            .expect("resumed recording");

        fs::remove_file(root.path().join("camera.flv")).expect("remove resumed path");
        drop(resumed);

        assert_eq!(store.stats(), RecordingStoreStats::default());
    }

    #[test]
    fn reports_partial_and_published_name_when_publication_rollback_fails() {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024),
                max_files: Some(4),
                max_active_recorders: 1,
            },
        )
        .expect("recording store");
        let mut recording = store.create("recording.flv").expect("recording");
        recording.write_all(b"recording").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
        recording
            .set_final_relative_name("camera.flv".to_owned())
            .expect("final recording name");
        recording.fail_publication_unlinks();

        let error = recording.commit().expect_err("injected rollback failure");

        assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
        assert_eq!(
            error.published_recording(),
            Some(&RecordingCommit {
                relative_name: "camera.flv".to_owned(),
                bytes: 9,
            })
        );
        assert!(root.path().join(&partial).is_file());
        assert!(root.path().join("camera.flv").is_file());
        assert_eq!(
            store.stats(),
            RecordingStoreStats {
                bytes_used: 18,
                files: 2,
                active_recorders: 0,
            }
        );
        assert!(matches!(
            error,
            RecordingStoreError::PublishRollback {
                recording: RecordingCommit { bytes: 9, .. },
                ..
            }
        ));
    }

    #[test]
    fn synchronizes_a_successful_publication_rollback_before_reporting_the_partial() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let mut recording = store.create("recording.flv").expect("recording");
        recording.write_all(b"recording").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
        recording
            .set_final_relative_name("camera.flv".to_owned())
            .expect("final recording name");
        recording.fail_partial_unlink();

        let error = recording
            .commit()
            .expect_err("injected partial unlink failure");

        assert!(matches!(error, RecordingStoreError::Publish { .. }));
        assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
        assert_eq!(error.published_recording(), None);
        assert!(root.path().join(partial).is_file());
        assert!(!root.path().join("camera.flv").exists());
        assert_eq!(
            store.stats(),
            RecordingStoreStats {
                bytes_used: 9,
                files: 1,
                active_recorders: 0,
            }
        );
    }

    #[test]
    fn reports_conservative_names_when_rollback_directory_sync_fails() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let mut recording = store.create("recording.flv").expect("recording");
        recording.write_all(b"recording").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
        recording
            .set_final_relative_name("camera.flv".to_owned())
            .expect("final recording name");
        recording.fail_partial_unlink_and_rollback_sync();

        let error = recording
            .commit()
            .expect_err("injected rollback directory sync failure");

        assert!(matches!(
            error,
            RecordingStoreError::PublishRollbackDirectorySync { .. }
        ));
        assert_eq!(error.recoverable_partial_name(), Some(partial.as_str()));
        assert_eq!(
            error.published_recording(),
            Some(&RecordingCommit {
                relative_name: "camera.flv".to_owned(),
                bytes: 9,
            })
        );
        assert!(root.path().join(partial).is_file());
        assert!(!root.path().join("camera.flv").exists());
        assert_eq!(
            store.stats(),
            RecordingStoreStats {
                bytes_used: 9,
                files: 1,
                active_recorders: 0,
            }
        );
    }

    fn test_store(root: &Path) -> RecordingStore {
        RecordingStore::open(root, test_limits()).expect("recording store")
    }

    fn test_limits() -> RecordingStoreLimits {
        RecordingStoreLimits {
            max_bytes: Some(1024),
            max_files: Some(4),
            max_active_recorders: 1,
        }
    }
}
