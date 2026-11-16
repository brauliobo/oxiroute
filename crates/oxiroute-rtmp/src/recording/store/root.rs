#[allow(clippy::wildcard_imports)]
use super::*;

/// A recording root opened once and retained by descriptor for its full lifetime.
#[derive(Clone)]
pub struct RecordingStore {
    pub(super) handle: Arc<RecordingStoreHandle>,
}

pub(super) struct RecordingStoreHandle {
    pub(super) incarnation: u64,
    shared: Mutex<Option<Arc<StoreShared>>>,
}

pub(super) struct StoreShared {
    pub(super) root: OwnedFd,
    root_owner: u32,
    pub(super) limits: RecordingStoreLimits,
    state: Mutex<StoreState>,
    incarnation_changed: Condvar,
    pub(super) finalizer: RecordingFinalizer,
    #[cfg(test)]
    operation_gate: Mutex<Option<Arc<RecordingOperationGate>>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IncarnationState {
    active: bool,
    claims: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct StoreState {
    pub(super) bytes_used: u64,
    pub(super) files: usize,
    pub(super) active_recorders: usize,
    incarnations: HashMap<u64, IncarnationState>,
}

pub(super) struct StoreOperationClaim {
    incarnation: u64,
    shared: Arc<StoreShared>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordingOperationGateKind {
    CreateBeforeOpen,
    RegisterBeforeWait,
    RegisterAfterWake,
    ResumeAfterSharedCapture,
}

#[cfg(test)]
pub(super) struct RecordingOperationGate {
    state: Mutex<RecordingOperationGateState>,
    changed: Condvar,
}

#[cfg(test)]
struct RecordingOperationGateState {
    kind: RecordingOperationGateKind,
    reached: bool,
    released: bool,
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

    pub(super) fn shared(&self) -> Result<Arc<StoreShared>, RecordingStoreError> {
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
    pub(super) fn install_operation_gate(
        &self,
        kind: RecordingOperationGateKind,
    ) -> Arc<RecordingOperationGate> {
        self.shared()
            .expect("active recording store")
            .install_operation_gate(kind)
    }
}

impl StoreShared {
    pub(super) fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().expect("recording store mutex poisoned")
    }

    #[cfg(test)]
    pub(super) fn install_operation_gate(
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
    pub(super) fn incarnation_count(&self) -> usize {
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

    pub(super) fn incarnation_active(&self, incarnation: u64) -> bool {
        self.lock()
            .incarnations
            .get(&incarnation)
            .is_some_and(|incarnation| incarnation.active)
    }

    pub(super) fn claim(self: &Arc<Self>, incarnation: u64) -> Option<StoreOperationClaim> {
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
    pub(super) fn wait_reached(&self, timeout: Duration) -> bool {
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

    pub(super) fn release(&self) {
        self.state
            .lock()
            .expect("recording operation gate state mutex poisoned")
            .released = true;
        self.changed.notify_all();
    }
}

impl StoreOperationClaim {
    pub(super) fn incarnation_active(&self) -> bool {
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

fn store_registry() -> &'static Mutex<HashMap<RootIdentity, Weak<StoreShared>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<RootIdentity, Weak<StoreShared>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_store_incarnation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}
