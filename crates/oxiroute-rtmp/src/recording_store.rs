use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::Duration,
};

use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags},
    io::Errno,
};
use uuid::Uuid;

use crate::{MAX_RECORDING_FILENAME_BYTES, recording_path::collision_recording_filename};

const PARTIAL_PREFIX: &str = ".oxiroute-recording-";
const PARTIAL_SUFFIX: &str = ".partial";
const OWNERSHIP_LOCK_NAME: &str = ".oxiroute-recording.lock";
const OWNER_PROBE_PREFIX: &str = ".oxiroute-owner-probe-";
const UUID_SIMPLE_LENGTH: usize = 32;
const MAX_NAME_ATTEMPTS: usize = 16;
const OWNERSHIP_RETRY_INTERVAL: Duration = Duration::from_millis(2);
const COMMIT_OPEN: u8 = 0;
const COMMIT_CANCELLED: u8 = 1;
const COMMIT_PUBLISHING: u8 = 2;
const COMMIT_FINISHED: u8 = 3;

/// Optional storage quotas and the hard active-recorder bound for one pinned recording root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingStoreLimits {
    pub max_bytes: Option<u64>,
    pub max_files: Option<usize>,
    pub max_active_recorders: usize,
}

/// Current use under one [`RecordingStore`].
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
    #[error("recording ownership lock cannot be opened as a no-follow regular file")]
    OwnershipOpen(#[source] io::Error),
    #[error("recording ownership lock cannot be acquired")]
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
    shared: Arc<StoreShared>,
}

struct StoreShared {
    root: OwnedFd,
    root_owner: u32,
    lock_identity: LockIdentity,
    limits: RecordingStoreLimits,
    state: Mutex<StoreState>,
}

#[derive(Clone, Copy, Debug, Default)]
struct StoreState {
    bytes_used: u64,
    files: usize,
    active_recorders: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LockIdentity {
    device: u64,
    inode: u64,
}

impl RecordingStore {
    /// Validates a recording root and its existing quota use without mutating the filesystem.
    ///
    /// This does not create the ownership lock, create an owner probe, remove partials, or sync the
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
        let root = open_pinned_directory(root.as_ref())
            .map_err(|source| RecordingStoreError::RootOpen(source.into()))?;
        let root_metadata = verify_root_control(&root)?;
        let root_owner = root_metadata.st_uid;
        let identity = RootIdentity {
            device: root_metadata.st_dev,
            inode: root_metadata.st_ino,
        };
        let ownership = open_ownership_lock(&root, root_owner)?;
        let lock_identity = lock_identity(&ownership)?;
        let mut registry = store_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(shared) = registry.get(&identity).and_then(Weak::upgrade) {
            if shared.lock_identity != lock_identity {
                return Err(invalid_ownership_lock());
            }
            if shared.limits != limits {
                return Err(RecordingStoreError::LimitsMismatch);
            }
            return Ok(Self { shared });
        }
        let clean_partials =
            match rustix_fs::flock(&ownership, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => true,
                Err(source) if source == Errno::AGAIN || source == Errno::WOULDBLOCK => false,
                Err(source) => return Err(RecordingStoreError::OwnershipLock(source.into())),
            };
        let state = scan_root(&root, limits, clean_partials)?;
        let shared = Arc::new(StoreShared {
            root,
            root_owner,
            lock_identity,
            limits,
            state: Mutex::new(state),
        });
        registry.insert(identity, Arc::downgrade(&shared));
        Ok(Self { shared })
    }

    /// Creates one exclusive hidden partial for a validated final relative name.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid name, exhausted quota, repeated partial-name collisions, or
    /// an operating-system creation failure.
    pub fn create(&self, final_relative_name: &str) -> Result<RecordingFile, RecordingStoreError> {
        self.create_inner(final_relative_name, || false)
    }

    pub(crate) fn create_unless(
        &self,
        final_relative_name: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<RecordingFile, RecordingStoreError> {
        self.create_inner(final_relative_name, cancelled)
    }

    fn create_inner(
        &self,
        final_relative_name: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<RecordingFile, RecordingStoreError> {
        validate_relative_name(final_relative_name)?;

        let ownership = acquire_shared_ownership(
            &self.shared.root,
            self.shared.root_owner,
            self.shared.lock_identity,
            &cancelled,
        )?;
        if cancelled() {
            return Err(RecordingStoreError::CreationCancelled);
        }
        let mut state = self.shared.lock();
        if state.active_recorders >= self.shared.limits.max_active_recorders {
            return Err(RecordingStoreError::ActiveRecorderLimit {
                maximum: self.shared.limits.max_active_recorders,
            });
        }
        if self
            .shared
            .limits
            .max_files
            .is_some_and(|maximum| state.files >= maximum)
        {
            return Err(RecordingStoreError::FileLimit {
                maximum: self.shared.limits.max_files.expect("checked file quota"),
            });
        }

        for _ in 0..MAX_NAME_ATTEMPTS {
            let partial_name = partial_name();
            match rustix_fs::openat(
                &self.shared.root,
                partial_name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(descriptor) => {
                    state.files += 1;
                    state.active_recorders += 1;
                    return Ok(RecordingFile {
                        shared: Arc::clone(&self.shared),
                        _ownership: ownership,
                        file: Some(File::from(descriptor)),
                        partial_name,
                        final_relative_name: final_relative_name.to_owned(),
                        position: 0,
                        length: 0,
                        partial_exists: true,
                        partial_exists_state: Arc::new(AtomicBool::new(true)),
                        active_accounted: true,
                        preserve_partial: Arc::new(AtomicBool::new(false)),
                        commit: Arc::new(RecordingCommitState {
                            state: AtomicU8::new(COMMIT_OPEN),
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
                    });
                }
                Err(Errno::EXIST) => {}
                Err(source) => return Err(RecordingStoreError::PartialCreate(source.into())),
            }
        }

        Err(RecordingStoreError::PartialNameCollisions)
    }

    #[must_use]
    pub fn stats(&self) -> RecordingStoreStats {
        let state = self.shared.lock();
        RecordingStoreStats {
            bytes_used: state.bytes_used,
            files: state.files,
            active_recorders: state.active_recorders,
        }
    }

    pub(crate) fn recording_names(&self) -> Result<Vec<String>, RecordingStoreError> {
        let mut directory = Dir::read_from(&self.shared.root)
            .map_err(|source| RecordingStoreError::RootRead(source.into()))?;
        let mut names = Vec::new();
        for entry in &mut directory {
            let entry = entry.map_err(|source| RecordingStoreError::RootRead(source.into()))?;
            let name = entry.file_name();
            let bytes = name.to_bytes();
            if bytes.starts_with(b".") {
                continue;
            }
            let metadata =
                match rustix_fs::statat(&self.shared.root, name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(metadata) => metadata,
                    Err(Errno::NOENT) => continue,
                    Err(source) => {
                        return Err(RecordingStoreError::RootEntryMetadata(source.into()));
                    }
                };
            if FileType::from_raw_mode(metadata.st_mode).is_file() {
                if let Ok(name) = std::str::from_utf8(bytes) {
                    names.push(name.to_owned());
                }
            }
        }
        Ok(names)
    }

    pub(crate) fn resume(
        &self,
        relative_name: &str,
    ) -> Result<RecordingResume, RecordingStoreError> {
        validate_relative_name(relative_name)?;
        let ownership = acquire_shared_ownership(
            &self.shared.root,
            self.shared.root_owner,
            self.shared.lock_identity,
            &|| false,
        )?;
        let descriptor = rustix_fs::openat(
            &self.shared.root,
            relative_name,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        let metadata = rustix_fs::fstat(&descriptor)
            .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        let path_metadata =
            rustix_fs::statat(&self.shared.root, relative_name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file()
            || metadata.st_uid != self.shared.root_owner
            || metadata.st_nlink != 1
            || metadata.st_dev != path_metadata.st_dev
            || metadata.st_ino != path_metadata.st_ino
        {
            return Err(RecordingStoreError::ResumeOpen(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording entry identity is invalid",
            )));
        }
        rustix_fs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive)
            .map_err(|source| RecordingStoreError::ResumeOpen(source.into()))?;
        let length =
            u64::try_from(metadata.st_size).map_err(|_| RecordingStoreError::ResumeInvalid)?;
        let mut file = File::from(descriptor);
        let (flags, last_timestamp_ms) = inspect_flv_tail(&mut file, length)?;
        let mut state = self.shared.lock();
        if state.active_recorders >= self.shared.limits.max_active_recorders {
            return Err(RecordingStoreError::ActiveRecorderLimit {
                maximum: self.shared.limits.max_active_recorders,
            });
        }
        state.active_recorders += 1;
        drop(state);
        Ok(RecordingResume {
            file: RecordingFile {
                shared: Arc::clone(&self.shared),
                _ownership: ownership,
                file: Some(file),
                partial_name: relative_name.to_owned(),
                final_relative_name: relative_name.to_owned(),
                position: length,
                length,
                partial_exists: false,
                partial_exists_state: Arc::new(AtomicBool::new(false)),
                active_accounted: true,
                preserve_partial: Arc::new(AtomicBool::new(false)),
                commit: Arc::new(RecordingCommitState {
                    state: AtomicU8::new(COMMIT_OPEN),
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
    pub const fn quota_scope(&self) -> RecordingQuotaScope {
        RecordingQuotaScope::Process
    }
}

/// Exclusive writable ownership of one hidden store partial.
pub struct RecordingFile {
    shared: Arc<StoreShared>,
    _ownership: OwnedFd,
    file: Option<File>,
    partial_name: String,
    final_relative_name: String,
    position: u64,
    length: u64,
    partial_exists: bool,
    partial_exists_state: Arc<AtomicBool>,
    active_accounted: bool,
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

    pub(crate) fn release_active_for_finalization(&mut self) {
        if !self.active_accounted {
            return;
        }
        self.shared.lock().active_recorders -= 1;
        self.active_accounted = false;
    }

    pub(crate) fn set_final_relative_name(
        &mut self,
        final_relative_name: String,
    ) -> Result<(), RecordingStoreError> {
        validate_relative_name(&final_relative_name)?;
        if self.resumed && self.final_relative_name != final_relative_name {
            return Err(RecordingStoreError::InvalidRelativeName);
        }
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

    /// Flushes and synchronizes the partial, closes it, then atomically publishes it without
    /// replacing an existing entry. A collision receives a bounded deterministic relative suffix.
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
        if let Some(file) = self.file.as_mut() {
            if let Err(source) = file.flush().and_then(|()| file.sync_all()) {
                self.commit.finish();
                return Err(RecordingStoreError::FileSync {
                    partial_relative_name: self.partial_name.clone(),
                    source,
                });
            }
        }
        if self.commit.cancelled() {
            return Err(self.cancelled_error());
        }
        if !self.partial_is_owned() {
            self.partial_exists = false;
            self.partial_exists_state.store(false, Ordering::Release);
            self.commit.finish();
            return Err(RecordingStoreError::PartialOwnershipLost {
                partial_relative_name: self.partial_name.clone(),
            });
        }
        if self.resumed {
            self.commit.finish();
            self.release_active_as_committed();
            return Ok(RecordingCommit {
                relative_name: self.final_relative_name.clone(),
                bytes: self.length,
            });
        }

        #[cfg(test)]
        self.commit.wait_before_publication();
        if !self.commit.begin_publication() {
            return Err(self.cancelled_error());
        }
        #[cfg(test)]
        self.commit.wait_after_publication_claim();
        let relative_name = match self.publish() {
            Ok(relative_name) => relative_name,
            Err(error) => {
                self.commit.finish();
                return Err(error);
            }
        };
        self.partial_exists = false;
        self.partial_exists_state.store(false, Ordering::Release);
        self.release_active_as_committed();
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

    fn release_active_as_committed(&mut self) {
        if !self.active_accounted {
            return;
        }
        let mut state = self.shared.lock();
        state.active_recorders -= 1;
        self.active_accounted = false;
    }

    fn account_duplicate_publication(&mut self) {
        let mut state = self.shared.lock();
        state.files = state.files.saturating_add(1);
        state.bytes_used = state.bytes_used.saturating_add(self.length);
        state.active_recorders -= 1;
        self.active_accounted = false;
    }
}

impl RecordingCommitState {
    fn cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == COMMIT_CANCELLED
    }

    fn begin_publication(&self) -> bool {
        self.state
            .compare_exchange(
                COMMIT_OPEN,
                COMMIT_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn finish(&self) {
        if self
            .state
            .compare_exchange(
                COMMIT_PUBLISHING,
                COMMIT_FINISHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            let _ = self.state.compare_exchange(
                COMMIT_OPEN,
                COMMIT_FINISHED,
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
                COMMIT_OPEN,
                COMMIT_CANCELLED,
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

fn inspect_flv_tail(file: &mut File, length: u64) -> Result<(u8, u32), RecordingStoreError> {
    if length < 28 {
        return Err(RecordingStoreError::ResumeInvalid);
    }
    let mut header = [0_u8; 9];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut header))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    if &header[..4] != b"FLV\x01" || header[5..9] != [0, 0, 0, 9] {
        return Err(RecordingStoreError::ResumeInvalid);
    }
    let mut previous_tag_size = [0_u8; 4];
    file.seek(SeekFrom::End(-4))
        .and_then(|_| file.read_exact(&mut previous_tag_size))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    let tag_size = u64::from(u32::from_be_bytes(previous_tag_size));
    let tag_start = length
        .checked_sub(4)
        .and_then(|end| end.checked_sub(tag_size))
        .filter(|start| *start >= 13)
        .ok_or(RecordingStoreError::ResumeInvalid)?;
    let mut tag_header = [0_u8; 11];
    file.seek(SeekFrom::Start(tag_start))
        .and_then(|_| file.read_exact(&mut tag_header))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    let data_size = u32::from_be_bytes([0, tag_header[1], tag_header[2], tag_header[3]]);
    if tag_size != u64::from(data_size) + 11 {
        return Err(RecordingStoreError::ResumeInvalid);
    }
    let timestamp_ms =
        u32::from_be_bytes([tag_header[7], tag_header[4], tag_header[5], tag_header[6]]);
    file.seek(SeekFrom::End(0))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    Ok((header[4], timestamp_ms))
}

impl Write for RecordingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "recording write is too large")
        })?;
        let requested_end = self.position.checked_add(requested).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "recording position overflow")
        })?;
        let reserved_growth = requested_end.saturating_sub(self.length);
        {
            let mut state = self.shared.lock();
            let Some(new_total) = state.bytes_used.checked_add(reserved_growth) else {
                return Err(byte_quota_error(
                    self.shared.limits.max_bytes.unwrap_or(u64::MAX),
                ));
            };
            if let Some(maximum) = self.shared.limits.max_bytes {
                if new_total > maximum {
                    return Err(byte_quota_error(maximum));
                }
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
        self.file
            .as_mut()
            .expect("recording file is writable until commit")
            .flush()
    }
}

impl Seek for RecordingFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
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
        if !self.active_accounted && !self.partial_exists {
            return;
        }
        if self.resumed {
            drop(self.file.take());
            if self.active_accounted {
                self.shared.lock().active_recorders -= 1;
            }
            self.active_accounted = false;
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
        if self.active_accounted {
            state.active_recorders -= 1;
        }
        if removed {
            state.files -= 1;
            state.bytes_used -= self.length;
        }
        if removed || !owned {
            self.partial_exists_state.store(false, Ordering::Release);
        }
        self.active_accounted = false;
    }
}

impl StoreShared {
    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().expect("recording store mutex poisoned")
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
        state.bytes_used = state.bytes_used.checked_add(size).unwrap_or(u64::MAX);
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

fn partial_name() -> String {
    format!(
        "{PARTIAL_PREFIX}{}{PARTIAL_SUFFIX}",
        Uuid::new_v4().simple()
    )
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

fn open_ownership_lock(root: &OwnedFd, root_owner: u32) -> Result<OwnedFd, RecordingStoreError> {
    let descriptor = rustix_fs::openat(
        root,
        OWNERSHIP_LOCK_NAME,
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|source| RecordingStoreError::OwnershipOpen(source.into()))?;
    let metadata = rustix_fs::fstat(&descriptor)
        .map_err(|source| RecordingStoreError::OwnershipOpen(source.into()))?;
    let path_metadata = rustix_fs::statat(root, OWNERSHIP_LOCK_NAME, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|source| RecordingStoreError::OwnershipOpen(source.into()))?;
    if !FileType::from_raw_mode(metadata.st_mode).is_file()
        || metadata.st_uid != root_owner
        || metadata.st_mode & 0o777 != 0o600
        || metadata.st_nlink != 1
        || metadata.st_dev != path_metadata.st_dev
        || metadata.st_ino != path_metadata.st_ino
    {
        return Err(invalid_ownership_lock());
    }
    Ok(descriptor)
}

fn acquire_shared_ownership(
    root: &OwnedFd,
    root_owner: u32,
    expected_identity: LockIdentity,
    cancelled: &impl Fn() -> bool,
) -> Result<OwnedFd, RecordingStoreError> {
    let ownership = open_ownership_lock(root, root_owner)?;
    if lock_identity(&ownership)? != expected_identity {
        return Err(invalid_ownership_lock());
    }
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

fn lock_identity(lock: &OwnedFd) -> Result<LockIdentity, RecordingStoreError> {
    let metadata = rustix_fs::fstat(lock)
        .map_err(|source| RecordingStoreError::OwnershipOpen(source.into()))?;
    Ok(LockIdentity {
        device: metadata.st_dev,
        inode: metadata.st_ino,
    })
}

fn invalid_ownership_lock() -> RecordingStoreError {
    RecordingStoreError::OwnershipOpen(io::Error::new(
        io::ErrorKind::InvalidData,
        "recording ownership entry identity or mode is invalid",
    ))
}

fn store_registry() -> &'static Mutex<HashMap<RootIdentity, Weak<StoreShared>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<RootIdentity, Weak<StoreShared>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn byte_quota_error(maximum: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        format!("recording byte limit of {maximum} reached"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        let mut recording = store.create("camera.flv").expect("recording partial");
        recording.write_all(b"recording").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
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
                rollback_source: _,
                ..
            }
        ));
    }

    #[test]
    fn synchronizes_a_successful_publication_rollback_before_reporting_the_partial() {
        let root = tempdir().expect("recording root");
        let store = test_store(root.path());
        let mut recording = store.create("camera.flv").expect("recording partial");
        recording.write_all(b"recording").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
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
        let mut recording = store.create("camera.flv").expect("recording partial");
        recording.write_all(b"recording").expect("recording data");
        let partial = recording.partial_relative_name().to_owned();
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
        RecordingStore::open(
            root,
            RecordingStoreLimits {
                max_bytes: Some(1024),
                max_files: Some(4),
                max_active_recorders: 1,
            },
        )
        .expect("recording store")
    }
}
