use std::{
    collections::HashMap,
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    path::{Component, Path},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
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
            Self::PublishedDirectorySync { recording, .. } => Some(recording),
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
                        active_accounted: true,
                        preserve_partial: Arc::new(AtomicBool::new(false)),
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
    active_accounted: bool,
    preserve_partial: Arc<AtomicBool>,
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

    pub(crate) fn set_final_relative_name(
        &mut self,
        final_relative_name: String,
    ) -> Result<(), RecordingStoreError> {
        validate_relative_name(&final_relative_name)?;
        self.final_relative_name = final_relative_name;
        Ok(())
    }

    /// Flushes and synchronizes the partial, closes it, then atomically publishes it without
    /// replacing an existing entry. A collision receives a bounded deterministic relative suffix.
    ///
    /// # Errors
    ///
    /// Returns an error when flushing, synchronizing, publishing, or synchronizing the containing
    /// directory fails. A directory-sync error carries the already-published recording.
    pub fn commit(mut self) -> Result<RecordingCommit, RecordingStoreError> {
        self.commit_inner(|| false)
    }

    pub(crate) fn commit_unless(
        mut self,
        cancelled: impl Fn() -> bool,
    ) -> Result<RecordingCommit, RecordingStoreError> {
        self.commit_inner(cancelled)
    }

    fn commit_inner(
        &mut self,
        cancelled: impl Fn() -> bool,
    ) -> Result<RecordingCommit, RecordingStoreError> {
        self.preserve_partial.store(true, Ordering::Release);
        if cancelled() {
            return Err(self.cancelled_error());
        }
        if let Some(file) = self.file.as_mut() {
            file.flush()
                .map_err(|source| RecordingStoreError::FileSync {
                    partial_relative_name: self.partial_name.clone(),
                    source,
                })?;
            file.sync_all()
                .map_err(|source| RecordingStoreError::FileSync {
                    partial_relative_name: self.partial_name.clone(),
                    source,
                })?;
        }
        if cancelled() {
            return Err(self.cancelled_error());
        }
        if !self.partial_is_owned() {
            self.partial_exists = false;
            return Err(RecordingStoreError::PartialOwnershipLost {
                partial_relative_name: self.partial_name.clone(),
            });
        }

        if cancelled() {
            return Err(self.cancelled_error());
        }
        let relative_name = self.publish()?;
        self.partial_exists = false;
        self.release_active_as_committed();
        let recording = RecordingCommit {
            relative_name,
            bytes: self.length,
        };
        if let Err(source) = rustix_fs::fsync(&self.shared.root) {
            return Err(RecordingStoreError::PublishedDirectorySync {
                recording,
                source: source.into(),
            });
        }
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
                        if let Err(source) = rustix_fs::unlinkat(
                            &self.shared.root,
                            self.partial_name.as_str(),
                            AtFlags::empty(),
                        ) {
                            let _ = rustix_fs::unlinkat(
                                &self.shared.root,
                                candidate.as_str(),
                                AtFlags::empty(),
                            );
                            return Err(RecordingStoreError::Publish {
                                partial_relative_name: self.partial_name.clone(),
                                source: source.into(),
                            });
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

    fn release_active_as_committed(&mut self) {
        if !self.active_accounted {
            return;
        }
        let mut state = self.shared.lock();
        state.active_recorders -= 1;
        self.active_accounted = false;
    }
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
        if !self.active_accounted {
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
        state.active_recorders -= 1;
        if removed {
            state.files -= 1;
            state.bytes_used -= self.length;
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
