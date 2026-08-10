use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
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

/// Persistent-store limits. `max_disk_bytes` and `max_disk_files` count recognized, durably
/// published cache records exactly; unrelated root entries are excluded. Admission reserves the
/// new record before creating one bounded, unpublished temporary record. During no-replace
/// publication, the temp and final names briefly refer to that same inode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskCacheConfig {
    pub memory: CacheConfig,
    pub max_disk_bytes: u64,
    pub max_disk_files: usize,
    pub max_record_bytes: usize,
}

impl Default for DiskCacheConfig {
    fn default() -> Self {
        Self {
            memory: CacheConfig::default(),
            max_disk_bytes: 512 * 1024 * 1024,
            max_disk_files: 10_000,
            max_record_bytes: 9 * 1024 * 1024,
        }
    }
}

/// Quota and locking scope of [`DiskCache`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskQuotaScope {
    /// One advisory, descriptor-held lease covers the root. A second cooperating `DiskCache`
    /// instance in this or another process is rejected, making its counters root-global. File mode
    /// and ownership exclude other users; a non-cooperating process running as the same user can
    /// bypass the advisory lease, so quotas are not distributed against such a process.
    ExclusiveRoot,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiskCacheStats {
    pub memory: CacheStats,
    pub disk_entries: usize,
    pub disk_bytes: u64,
    pub recovered: u64,
    pub stale_temps_removed: u64,
    pub corrupt_records_removed: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DiskCacheError {
    #[error("invalid persistent cache configuration")]
    InvalidConfig,
    #[error("persistent cache root cannot be opened or created without following symlinks")]
    RootOpen(#[source] io::Error),
    #[error("persistent cache root must be owned by this user and have mode 0700")]
    RootControl,
    #[error("persistent cache ownership lock is unsafe or cannot be opened")]
    OwnershipOpen(#[source] io::Error),
    #[error("persistent cache root is already owned by another instance")]
    AlreadyOwned,
    #[error("persistent cache ownership lock cannot be acquired")]
    OwnershipLock(#[source] io::Error),
    #[error("persistent cache root cannot be enumerated")]
    RootRead(#[source] io::Error),
    #[error("persistent cache record filesystem operation failed")]
    Storage(#[source] io::Error),
    #[error("persistent cache record exceeds configured disk bounds")]
    RecordTooLarge,
    #[error("persistent cache record cannot fit the configured quota")]
    Quota,
    #[error("persistent cache namespace contains an entry that cannot be safely removed")]
    UnsafeEntry,
    #[error("persistent cache is shut down")]
    Closed,
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[cfg(test)]
    #[error("injected persistent cache fault")]
    Injected,
}

/// Shared HTTP cache whose accepted representations and LRU order survive process restart.
#[derive(Clone)]
pub struct DiskCache {
    shared: Arc<Shared>,
}

struct Shared {
    cache: Cache,
    config: DiskCacheConfig,
    root: OwnedFd,
    root_owner: u32,
    _ownership: OwnedFd,
    state: Mutex<State>,
    closed: AtomicBool,
    #[cfg(test)]
    fault: Mutex<Option<FaultPoint>>,
}

struct State {
    records: HashMap<CacheKey, RecordInfo>,
    tags: HashMap<Bytes, BTreeSet<CacheKey>>,
    bytes_used: u64,
    sequence: u64,
    recovered: u64,
    stale_temps_removed: u64,
    corrupt_records_removed: u64,
}

#[derive(Clone)]
struct RecordInfo {
    name: String,
    size: u64,
    device: u64,
    inode: u64,
    access: u64,
    sequence: u64,
    tags: Vec<Bytes>,
}

struct DecodedRecord {
    entry: RecoveredEntry,
    access: u64,
    sequence: u64,
}

pub enum DiskFillJoin {
    Leader(DiskFillGuard),
    Follower(FillWaiter),
    AtCapacity,
}

pub struct DiskFillGuard {
    shared: Arc<Shared>,
    inner: Option<FillGuard>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    AfterFileSync,
    AfterPublish,
    BeforeDirectorySync,
    BeforeMemoryReconciliation,
}

impl DiskCache {
    /// Opens or creates a mode-0700 cache root, acquires its exclusive lifetime lease, removes stale
    /// owned temps, validates all records, and recovers the memory cache and tag index.
    ///
    /// Every path component is opened with `O_NOFOLLOW`. Only the final component may be created;
    /// missing parent directories are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid limits, unsafe path ownership/modes, another active owner, or
    /// an I/O failure that prevents deterministic recovery.
    pub fn open(root: impl AsRef<Path>, config: DiskCacheConfig) -> Result<Self, DiskCacheError> {
        Self::with_clock(root, config, Arc::new(SystemClock::new()))
    }

    /// Opens a persistent cache with an injected monotonic clock.
    ///
    /// # Errors
    ///
    /// Returns the same root, ownership, recovery, configuration, and I/O errors as [`Self::open`].
    pub fn with_clock(
        root: impl AsRef<Path>,
        config: DiskCacheConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, DiskCacheError> {
        validate_disk_config(&config)?;
        let cache = Cache::with_clock(config.memory.clone(), clock)?;
        let root = open_or_create_root(root.as_ref())?;
        let root_owner = verify_root_control(&root)?;
        let ownership = open_ownership_lock(&root, root_owner)?;
        match rustix_fs::flock(&ownership, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(source) if source == Errno::AGAIN || source == Errno::WOULDBLOCK => {
                return Err(DiskCacheError::AlreadyOwned);
            }
            Err(source) => return Err(DiskCacheError::OwnershipLock(source.into())),
        }

        let state = scan_root(&root, root_owner, &config, &cache)?;
        let shared = Arc::new(Shared {
            cache,
            config,
            root,
            root_owner,
            _ownership: ownership,
            state: Mutex::new(state),
            closed: AtomicBool::new(false),
            #[cfg(test)]
            fault: Mutex::new(None),
        });
        Ok(Self { shared })
    }

    #[must_use]
    pub fn config(&self) -> &DiskCacheConfig {
        &self.shared.config
    }

    #[must_use]
    pub const fn quota_scope(&self) -> DiskQuotaScope {
        DiskQuotaScope::ExclusiveRoot
    }

    #[must_use]
    pub fn now(&self) -> MonoTime {
        self.shared.cache.now()
    }

    /// Validates an upstream response with the memory cache's policy and bounds.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or when memory-cache policy rejects the response.
    pub fn prepare(
        &self,
        request: RequestKeyInput<'_>,
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
        timing: ResponseTiming,
        tags: &[&[u8]],
    ) -> Result<PreparedEntry, DiskCacheError> {
        self.ensure_open()?;
        self.shared
            .cache
            .prepare(request, status, headers, body, timing, tags)
            .map_err(Into::into)
    }

    /// Validates an upstream response with the memory cache's policy and a canonical timeline.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or when the response is not safe or bounded.
    pub fn prepare_with_timeline(
        &self,
        request: RequestKeyInput<'_>,
        response: crate::CacheResponse<'_>,
        timeline: &crate::CacheTimeline,
    ) -> Result<PreparedEntry, DiskCacheError> {
        self.ensure_open()?;
        self.shared
            .cache
            .prepare_with_timeline(request, response, timeline)
            .map_err(Into::into)
    }

    /// Prepares a replacement by applying 304 metadata to the resident representation.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or for an invalid or missing representation.
    pub fn prepare_not_modified(
        &self,
        request: RequestKeyInput<'_>,
        key: &CacheKey,
        not_modified: &HeaderMap,
        timing: ResponseTiming,
    ) -> Result<PreparedEntry, DiskCacheError> {
        self.ensure_open()?;
        self.shared
            .cache
            .prepare_not_modified(request, key, not_modified, timing)
            .map_err(Into::into)
    }

    /// Prepares a persistent replacement by applying 304 metadata to the resident representation
    /// using the same canonical timeline as the original response.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or for an invalid or missing representation.
    pub fn prepare_not_modified_with_timeline(
        &self,
        request: RequestKeyInput<'_>,
        key: &CacheKey,
        not_modified: &HeaderMap,
        timing: ResponseTiming,
        timeline: &crate::CacheTimeline,
    ) -> Result<PreparedEntry, DiskCacheError> {
        self.ensure_open()?;
        self.shared
            .cache
            .prepare_not_modified_with_timeline(request, key, not_modified, timing, timeline)
            .map_err(Into::into)
    }

    /// Looks up a representation and durably advances its LRU position on a reusable result.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid request metadata, shutdown, or failed durable LRU publication.
    pub fn lookup(&self, request: RequestKeyInput<'_>) -> Result<Lookup, DiskCacheError> {
        self.ensure_open()?;
        let mut state = self.shared.lock();
        self.ensure_open()?;
        let result = self.shared.cache.lookup(request)?;
        let key = match &result {
            Lookup::Hit { response, .. } | Lookup::Revalidate { response, .. } => {
                Some(response.key.clone())
            }
            Lookup::Bypass { .. } | Lookup::Miss { .. } => None,
        };
        if let Some(key) = key {
            self.shared.touch(&mut state, &key)?;
        } else if let Lookup::Miss { base, .. } = &result {
            self.shared.reconcile_base(&mut state, base)?;
        }
        Ok(result)
    }

    /// Returns an eligible stale-if-error response and durably advances its LRU position.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or when durable LRU publication fails.
    pub fn stale_if_error(&self, key: &CacheKey) -> Result<Option<CachedResponse>, DiskCacheError> {
        self.ensure_open()?;
        let mut state = self.shared.lock();
        self.ensure_open()?;
        let response = self.shared.cache.stale_if_error(key);
        if response.is_some() {
            self.shared.touch(&mut state, key)?;
        }
        Ok(response)
    }

    /// Starts or joins the memory cache's bounded collapsed-forwarding generation.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or for an invalid fill key.
    pub fn begin_fill(&self, base: BaseKey) -> Result<DiskFillJoin, DiskCacheError> {
        self.ensure_open()?;
        let _state = self.shared.lock();
        self.ensure_open()?;
        match self.shared.cache.begin_fill(base)? {
            FillJoin::Leader(inner) => Ok(DiskFillJoin::Leader(DiskFillGuard {
                shared: Arc::clone(&self.shared),
                inner: Some(inner),
            })),
            FillJoin::Follower(waiter) => Ok(DiskFillJoin::Follower(waiter)),
            FillJoin::AtCapacity => Ok(DiskFillJoin::AtCapacity),
        }
    }

    /// Durably removes one exact representation before cancelling its in-process fill.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or if identity-checked removal or directory sync fails.
    pub fn purge_exact(&self, key: &CacheKey) -> Result<PurgeResult, DiskCacheError> {
        self.ensure_open()?;
        let mut state = self.shared.lock();
        self.ensure_open()?;
        if let Some(record) = state.records.get(key).cloned() {
            self.shared.remove_record(&record)?;
            state.remove(key);
            self.shared.sync_root()?;
        }
        Ok(self.shared.cache.purge_exact(key))
    }

    /// Durably removes every representation for one bounded request key before cancelling its fill.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown or if identity-checked removal or directory sync fails.
    pub fn purge_base(&self, base: &BaseKey) -> Result<PurgeResult, DiskCacheError> {
        self.ensure_open()?;
        let mut state = self.shared.lock();
        self.ensure_open()?;
        let keys = state
            .records
            .keys()
            .filter(|key| key.base() == base)
            .cloned()
            .collect::<Vec<_>>();
        let mut removed = 0usize;
        for key in &keys {
            if let Some(record) = state.records.get(key).cloned() {
                self.shared.remove_record(&record)?;
                state.remove(key);
                removed += 1;
            }
        }
        if removed != 0 {
            self.shared.sync_root()?;
        }
        Ok(self.shared.cache.purge_base(base))
    }

    /// Durably removes all representations in the recovered tag index.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid tag, shutdown, unsafe namespace entry, or sync failure.
    pub fn purge_tag(&self, tag: &[u8]) -> Result<PurgeResult, DiskCacheError> {
        self.ensure_open()?;
        validate_tag(tag, self.shared.config.memory.max_tag_bytes)?;
        let mut state = self.shared.lock();
        self.ensure_open()?;
        let keys = state.tags.get(tag).cloned().unwrap_or_default();
        let mut removed = 0usize;
        for key in &keys {
            if let Some(record) = state.records.get(key).cloned() {
                self.shared.remove_record(&record)?;
                state.remove(key);
                removed += 1;
            }
        }
        if removed != 0 {
            self.shared.sync_root()?;
        }
        self.shared.cache.purge_tag(tag).map_err(Into::into)
    }

    #[must_use]
    pub fn stats(&self) -> DiskCacheStats {
        let state = self.shared.lock();
        DiskCacheStats {
            memory: self.shared.cache.stats(),
            disk_entries: state.records.len(),
            disk_bytes: state.bytes_used,
            recovered: state.recovered,
            stale_temps_removed: state.stale_temps_removed,
            corrupt_records_removed: state.corrupt_records_removed,
        }
    }

    /// Stops admission, cancels at most the configured number of in-process fills, and performs one
    /// root-directory sync. It never waits for readers or fill leaders. Work is bounded, but the
    /// filesystem controls how long `fsync` itself may block. The root lease is released when the
    /// final cache handle and fill guard are dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the final directory synchronization fails.
    pub fn shutdown(&self) -> Result<(), DiskCacheError> {
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let _state = self.shared.lock();
        self.shared.cache.cancel_all_fills();
        rustix_fs::fsync(&self.shared.root).map_err(storage)
    }

    fn ensure_open(&self) -> Result<(), DiskCacheError> {
        if self.shared.closed.load(Ordering::Acquire) {
            Err(DiskCacheError::Closed)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn inject(&self, point: FaultPoint) {
        *self
            .shared
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(point);
    }
}

impl DiskFillGuard {
    /// Publishes the record durably, then makes it visible through the memory cache.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign entry, mismatched generation, quota failure, shutdown, or
    /// storage failure.
    pub fn store(mut self, entry: PreparedEntry) -> Result<StoreOutcome, DiskCacheError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(DiskCacheError::Closed);
        }
        let Some(inner) = self.inner.take() else {
            return Ok(StoreOutcome::GenerationLost);
        };
        let entry = inner.claim(entry)?;
        if !inner.is_current() {
            return Ok(StoreOutcome::GenerationLost);
        }

        let mut state = self.shared.lock();
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(DiskCacheError::Closed);
        }
        if !inner.is_current() {
            return Ok(StoreOutcome::GenerationLost);
        }
        let sequence = state.next_sequence();
        let access = sequence;
        let encoded = encode_record(
            entry.prepared(),
            sequence,
            access,
            self.shared.cache.now(),
            SystemTime::now(),
        )?;
        if encoded.len() > self.shared.config.max_record_bytes {
            return Err(DiskCacheError::RecordTooLarge);
        }
        let encoded_size =
            u64::try_from(encoded.len()).map_err(|_| DiskCacheError::RecordTooLarge)?;
        let victims =
            admission_victims(&state, entry.prepared(), encoded_size, &self.shared.config)?;
        for key in &victims {
            if let Some(record) = state.records.get(key).cloned() {
                self.shared.remove_record(&record)?;
                state.remove(key);
                self.shared.cache.remove_without_cancelling_fill(key);
            }
        }
        if !victims.is_empty() {
            self.shared.sync_root()?;
        }

        let record = self
            .shared
            .publish(&encoded, sequence, access, &entry.prepared().tags)?;
        let stored_key = entry.prepared().key.clone();
        state.insert(stored_key.clone(), record);
        let delta = match inner.store_claimed(entry) {
            ClaimedStoreOutcome::Stored(delta) => delta,
            ClaimedStoreOutcome::GenerationLost => {
                if let Some(record) = state.records.get(&stored_key).cloned() {
                    self.shared.remove_record(&record)?;
                    state.remove(&stored_key);
                    self.shared.sync_root()?;
                }
                return Ok(StoreOutcome::GenerationLost);
            }
        };
        let reconciliation = (|| {
            #[cfg(test)]
            self.shared.fault(FaultPoint::BeforeMemoryReconciliation)?;
            let removed = reconcile_insertion(&self.shared.root, &mut state, &delta)?;
            if removed != 0 {
                self.shared.sync_root()?;
            }
            Ok(removed)
        })();
        let reconciled = self.shared.durability(reconciliation)?;
        Ok(StoreOutcome::Stored {
            evicted: victims.len().saturating_add(reconciled),
        })
    }

    #[must_use]
    pub fn complete_without_store(mut self) -> bool {
        self.inner
            .take()
            .is_some_and(FillGuard::complete_without_store)
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn touch(&self, state: &mut State, key: &CacheKey) -> Result<(), DiskCacheError> {
        let Some(old) = state.records.get(key).cloned() else {
            return Ok(());
        };
        let Some(entry) = self.cache.entry(key) else {
            return Ok(());
        };
        let sequence = state.next_sequence();
        let encoded = encode_record(
            &entry,
            sequence,
            sequence,
            self.cache.now(),
            SystemTime::now(),
        )?;
        if encoded.len() > self.config.max_record_bytes {
            return Err(DiskCacheError::RecordTooLarge);
        }
        self.remove_record(&old)?;
        self.sync_root()?;
        state.remove(key);
        let new = self.publish(&encoded, sequence, sequence, &entry.tags)?;
        state.insert(key.clone(), new);
        Ok(())
    }

    fn reconcile_base(&self, state: &mut State, base: &BaseKey) -> Result<(), DiskCacheError> {
        let keys = state
            .records
            .keys()
            .filter(|key| key.base() == base && self.cache.entry(key).is_none())
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            if let Some(record) = state.records.get(key).cloned() {
                self.remove_record(&record)?;
                state.remove(key);
            }
        }
        if !keys.is_empty() {
            self.sync_root()?;
        }
        Ok(())
    }

    fn publish(
        &self,
        bytes: &[u8],
        sequence: u64,
        access: u64,
        tags: &[Bytes],
    ) -> Result<RecordInfo, DiskCacheError> {
        let (temp_name, descriptor) = self.create_temp()?;
        let mut file = File::from(descriptor);
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = remove_owned_name(&self.root, &temp_name, None);
            return Err(storage(source));
        }
        #[cfg(test)]
        self.fault(FaultPoint::AfterFileSync)?;
        let temp_metadata = rustix_fs::fstat(&file).map_err(storage)?;
        verify_named_identity(&self.root, &temp_name, &temp_metadata, self.root_owner)?;

        let final_name = format!(
            "{RECORD_PREFIX}{sequence:016x}-{}{RECORD_SUFFIX}",
            Uuid::new_v4().simple()
        );
        if let Err(source) =
            rustix_fs::linkat(&file, "", &self.root, &final_name, AtFlags::EMPTY_PATH)
        {
            let _ = remove_owned_name(
                &self.root,
                &temp_name,
                Some((temp_metadata.st_dev, temp_metadata.st_ino)),
            );
            return Err(storage(source));
        }
        #[cfg(test)]
        self.durability(self.fault(FaultPoint::AfterPublish))?;
        self.durability(remove_owned_name(
            &self.root,
            &temp_name,
            Some((temp_metadata.st_dev, temp_metadata.st_ino)),
        ))?;
        #[cfg(test)]
        self.durability(self.fault(FaultPoint::BeforeDirectorySync))?;
        self.sync_root()?;
        let final_metadata = self.durability(
            rustix_fs::statat(&self.root, &final_name, AtFlags::SYMLINK_NOFOLLOW).map_err(storage),
        )?;
        if final_metadata.st_dev != temp_metadata.st_dev
            || final_metadata.st_ino != temp_metadata.st_ino
            || final_metadata.st_nlink != 1
        {
            self.closed.store(true, Ordering::Release);
            return Err(DiskCacheError::UnsafeEntry);
        }
        Ok(RecordInfo {
            name: final_name,
            size: u64::try_from(bytes.len()).map_err(|_| DiskCacheError::RecordTooLarge)?,
            device: final_metadata.st_dev,
            inode: final_metadata.st_ino,
            access,
            sequence,
            tags: tags.to_vec(),
        })
    }

    fn create_temp(&self) -> Result<(String, OwnedFd), DiskCacheError> {
        for _ in 0..MAX_NAME_ATTEMPTS {
            let name = format!("{TEMP_PREFIX}{}{TEMP_SUFFIX}", Uuid::new_v4().simple());
            match rustix_fs::openat(
                &self.root,
                &name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(descriptor) => return Ok((name, descriptor)),
                Err(Errno::EXIST) => {}
                Err(source) => return Err(storage(source)),
            }
        }
        Err(DiskCacheError::Storage(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary-name collision limit reached",
        )))
    }

    fn remove_record(&self, record: &RecordInfo) -> Result<(), DiskCacheError> {
        remove_owned_name(
            &self.root,
            &record.name,
            Some((record.device, record.inode)),
        )
    }

    fn sync_root(&self) -> Result<(), DiskCacheError> {
        self.durability(rustix_fs::fsync(&self.root).map_err(storage))
    }

    fn durability<T>(&self, result: Result<T, DiskCacheError>) -> Result<T, DiskCacheError> {
        if result.is_err() {
            self.closed.store(true, Ordering::Release);
        }
        result
    }

    #[cfg(test)]
    fn fault(&self, point: FaultPoint) -> Result<(), DiskCacheError> {
        let mut fault = self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *fault == Some(point) {
            *fault = None;
            return Err(DiskCacheError::Injected);
        }
        Ok(())
    }
}

impl State {
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    fn insert(&mut self, key: CacheKey, record: RecordInfo) {
        self.bytes_used = self.bytes_used.saturating_add(record.size);
        for tag in &record.tags {
            self.tags
                .entry(tag.clone())
                .or_default()
                .insert(key.clone());
        }
        self.records.insert(key, record);
    }

    fn remove(&mut self, key: &CacheKey) -> Option<RecordInfo> {
        let record = self.records.remove(key)?;
        self.bytes_used = self.bytes_used.saturating_sub(record.size);
        for tag in &record.tags {
            if let Some(keys) = self.tags.get_mut(tag) {
                keys.remove(key);
                if keys.is_empty() {
                    self.tags.remove(tag);
                }
            }
        }
        Some(record)
    }
}

fn admission_victims(
    state: &State,
    entry: &PreparedEntry,
    size: u64,
    config: &DiskCacheConfig,
) -> Result<Vec<CacheKey>, DiskCacheError> {
    if size > config.max_disk_bytes {
        return Err(DiskCacheError::Quota);
    }
    let mut victims = state
        .records
        .keys()
        .filter(|key| {
            *key == &entry.key
                || key.base() == entry.key.base() && !key.same_vary_schema(&entry.key)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    loop {
        let removed_bytes = victims.iter().fold(0u64, |total, key| {
            total.saturating_add(state.records.get(key).map_or(0, |record| record.size))
        });
        let remaining = state.records.len().saturating_sub(victims.len());
        let bytes = state.bytes_used.saturating_sub(removed_bytes);
        if remaining < config.max_disk_files && bytes.saturating_add(size) <= config.max_disk_bytes
        {
            break;
        }
        let next = state
            .records
            .iter()
            .filter(|(key, _)| !victims.contains(*key))
            .min_by(|(left_key, left), (right_key, right)| {
                (left.access, left.sequence, *left_key).cmp(&(
                    right.access,
                    right.sequence,
                    *right_key,
                ))
            })
            .map(|(key, _)| key.clone())
            .ok_or(DiskCacheError::Quota)?;
        victims.insert(next);
    }
    Ok(victims.into_iter().collect())
}

fn validate_disk_config(config: &DiskCacheConfig) -> Result<(), DiskCacheError> {
    if config.max_disk_bytes == 0
        || config.max_disk_files == 0
        || config.max_record_bytes == 0
        || u64::try_from(config.max_record_bytes)
            .map_or(true, |maximum| maximum > config.max_disk_bytes)
    {
        return Err(DiskCacheError::InvalidConfig);
    }
    Ok(())
}

fn open_or_create_root(path: &Path) -> Result<OwnedFd, DiskCacheError> {
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(DiskCacheError::RootOpen(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty cache root",
        )));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let anchor = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut directory = rustix_fs::open(anchor, flags, Mode::empty())
        .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
    let normals = components
        .iter()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(*name),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(DiskCacheError::RootOpen(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parent traversal is not allowed",
        )));
    }
    for (index, name) in normals.iter().enumerate() {
        match rustix_fs::openat(&directory, *name, flags, Mode::empty()) {
            Ok(next) => directory = next,
            Err(Errno::NOENT) if index + 1 == normals.len() => {
                rustix_fs::mkdirat(&directory, *name, Mode::from_raw_mode(ROOT_MODE))
                    .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
                rustix_fs::fsync(&directory)
                    .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
                directory = rustix_fs::openat(&directory, *name, flags, Mode::empty())
                    .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
            }
            Err(source) => return Err(DiskCacheError::RootOpen(source.into())),
        }
    }
    Ok(directory)
}

fn verify_root_control(root: &OwnedFd) -> Result<u32, DiskCacheError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| DiskCacheError::RootControl)?;
    if !FileType::from_raw_mode(metadata.st_mode).is_dir() || metadata.st_mode & 0o777 != ROOT_MODE
    {
        return Err(DiskCacheError::RootControl);
    }
    for _ in 0..MAX_NAME_ATTEMPTS {
        let name = format!("{OWNER_PROBE_PREFIX}{}", Uuid::new_v4().simple());
        match rustix_fs::openat(
            root,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(probe) => {
                let probe = rustix_fs::fstat(&probe).map_err(|_| DiskCacheError::RootControl)?;
                rustix_fs::unlinkat(root, &name, AtFlags::empty())
                    .map_err(|_| DiskCacheError::RootControl)?;
                if probe.st_uid != metadata.st_uid {
                    return Err(DiskCacheError::RootControl);
                }
                return Ok(metadata.st_uid);
            }
            Err(Errno::EXIST) => {}
            Err(_) => return Err(DiskCacheError::RootControl),
        }
    }
    Err(DiskCacheError::RootControl)
}

fn open_ownership_lock(root: &OwnedFd, owner: u32) -> Result<OwnedFd, DiskCacheError> {
    let (descriptor, created) = match rustix_fs::openat(
        root,
        LOCK_NAME,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    ) {
        Ok(descriptor) => (descriptor, true),
        Err(Errno::EXIST) => (
            rustix_fs::openat(
                root,
                LOCK_NAME,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?,
            false,
        ),
        Err(source) => return Err(DiskCacheError::OwnershipOpen(source.into())),
    };
    if created {
        rustix_fs::fchmod(&descriptor, Mode::from_raw_mode(FILE_MODE))
            .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    }
    let metadata = rustix_fs::fstat(&descriptor)
        .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    let linked = rustix_fs::statat(root, LOCK_NAME, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    if !secure_file(&metadata, owner)
        || metadata.st_dev != linked.st_dev
        || metadata.st_ino != linked.st_ino
    {
        return Err(DiskCacheError::OwnershipOpen(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership lock identity, type, owner, links, or mode is invalid",
        )));
    }
    if created {
        rustix_fs::fsync(&descriptor)
            .and_then(|()| rustix_fs::fsync(root))
            .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    }
    Ok(descriptor)
}

#[allow(clippy::too_many_lines)]
fn scan_root(
    root: &OwnedFd,
    owner: u32,
    config: &DiskCacheConfig,
    cache: &Cache,
) -> Result<State, DiskCacheError> {
    let mut directory =
        Dir::read_from(root).map_err(|source| DiskCacheError::RootRead(source.into()))?;
    let mut candidates: HashMap<CacheKey, (DecodedRecord, RecordInfo)> = HashMap::new();
    let mut cleaned = false;
    let mut stale_temps_removed = 0u64;
    let mut corrupt_records_removed = 0u64;
    for entry in &mut directory {
        let entry = entry.map_err(|source| DiskCacheError::RootRead(source.into()))?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") || raw == LOCK_NAME.as_bytes() || is_owner_probe(raw) {
            continue;
        }
        let Ok(name) = std::str::from_utf8(raw) else {
            continue;
        };
        let temp = is_temp_name(raw);
        let record = is_record_name(raw);
        if !temp && !record {
            continue;
        }
        let metadata = match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(Errno::NOENT) => continue,
            Err(source) => return Err(storage(source)),
        };
        if temp {
            remove_namespace_entry(root, name, &metadata)?;
            stale_temps_removed = stale_temps_removed.saturating_add(1);
            cleaned = true;
            continue;
        }
        let decoded = read_record(root, name, &metadata, owner, config, cache);
        let Ok(decoded) = decoded else {
            remove_namespace_entry(root, name, &metadata)?;
            corrupt_records_removed = corrupt_records_removed.saturating_add(1);
            cleaned = true;
            continue;
        };
        let size = u64::try_from(metadata.st_size).map_err(|_| DiskCacheError::RecordTooLarge)?;
        let info = RecordInfo {
            name: name.to_owned(),
            size,
            device: metadata.st_dev,
            inode: metadata.st_ino,
            access: decoded.access,
            sequence: decoded.sequence,
            tags: decoded.entry.tags.clone(),
        };
        match candidates.entry(decoded.entry.key.clone()) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert((decoded, info));
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                let old_order = (
                    slot.get().0.access,
                    slot.get().0.sequence,
                    slot.get().1.name.as_str(),
                );
                let new_order = (decoded.access, decoded.sequence, info.name.as_str());
                if new_order > old_order {
                    let old = &slot.get().1;
                    remove_owned_name(root, &old.name, Some((old.device, old.inode)))?;
                    slot.insert((decoded, info));
                } else {
                    remove_owned_name(root, name, Some((metadata.st_dev, metadata.st_ino)))?;
                }
                corrupt_records_removed = corrupt_records_removed.saturating_add(1);
                cleaned = true;
            }
        }
    }

    let mut ordered = candidates.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (left.0.access, left.0.sequence, &left.0.entry.key).cmp(&(
            right.0.access,
            right.0.sequence,
            &right.0.entry.key,
        ))
    });
    while ordered.len() > config.max_disk_files
        || ordered.iter().map(|(_, info)| info.size).sum::<u64>() > config.max_disk_bytes
    {
        let (_, info) = ordered.remove(0);
        remove_owned_name(root, &info.name, Some((info.device, info.inode)))?;
        corrupt_records_removed = corrupt_records_removed.saturating_add(1);
        cleaned = true;
    }
    if cleaned {
        rustix_fs::fsync(root).map_err(storage)?;
    }
    let mut state = State {
        records: HashMap::new(),
        tags: HashMap::new(),
        bytes_used: 0,
        sequence: 0,
        recovered: ordered.len() as u64,
        stale_temps_removed,
        corrupt_records_removed,
    };
    let mut memory_removed = 0usize;
    for (decoded, info) in ordered {
        state.sequence = state.sequence.max(decoded.sequence).max(decoded.access);
        let key = decoded.entry.key.clone();
        let delta = cache.restore(decoded.entry);
        state.insert(key, info);
        memory_removed =
            memory_removed.saturating_add(reconcile_insertion(root, &mut state, &delta)?);
    }
    if memory_removed != 0 {
        rustix_fs::fsync(root).map_err(storage)?;
    }
    Ok(state)
}

fn reconcile_insertion(
    root: &OwnedFd,
    state: &mut State,
    delta: &InsertionDelta,
) -> Result<usize, DiskCacheError> {
    let mut removed = 0usize;
    for key in &delta.removed {
        if key == &delta.inserted {
            continue;
        }
        if let Some(record) = state.records.get(key).cloned() {
            remove_owned_name(root, &record.name, Some((record.device, record.inode)))?;
            state.remove(key);
            removed += 1;
        }
    }
    Ok(removed)
}

fn read_record(
    root: &OwnedFd,
    name: &str,
    path_metadata: &rustix_fs::Stat,
    owner: u32,
    config: &DiskCacheConfig,
    cache: &Cache,
) -> Result<DecodedRecord, DiskCacheError> {
    if !secure_file(path_metadata, owner) {
        return Err(DiskCacheError::UnsafeEntry);
    }
    let descriptor = rustix_fs::openat(
        root,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(storage)?;
    let before = rustix_fs::fstat(&descriptor).map_err(storage)?;
    if before.st_dev != path_metadata.st_dev || before.st_ino != path_metadata.st_ino {
        return Err(DiskCacheError::UnsafeEntry);
    }
    let size = usize::try_from(before.st_size).map_err(|_| DiskCacheError::RecordTooLarge)?;
    if size > config.max_record_bytes {
        return Err(DiskCacheError::RecordTooLarge);
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(size);
    Read::by_ref(&mut file)
        .take((config.max_record_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    let after = rustix_fs::fstat(&file).map_err(storage)?;
    if bytes.len() != size
        || before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_size != after.st_size
    {
        return Err(DiskCacheError::UnsafeEntry);
    }
    decode_record(&bytes, config, cache.now())
}

fn encode_record(
    entry: &PreparedEntry,
    sequence: u64,
    access: u64,
    now: MonoTime,
    wall: SystemTime,
) -> Result<Vec<u8>, DiskCacheError> {
    let mut payload = Encoder::default();
    payload.u64(sequence);
    payload.u64(access);
    payload.duration(
        entry
            .policy
            .corrected_initial_age
            .saturating_add(now.saturating_duration_since(entry.response_received)),
    );
    payload.duration(wall.duration_since(UNIX_EPOCH).unwrap_or_default());
    payload.bytes(entry.key.base.method.as_str().as_bytes())?;
    payload.bytes(entry.key.base.scheme.as_bytes())?;
    payload.bytes(entry.key.base.authority.as_bytes())?;
    payload.bytes(entry.key.base.path.as_bytes())?;
    payload.optional_bytes(entry.key.base.query.as_deref().map(str::as_bytes))?;
    payload.u32_len(entry.key.vary.len())?;
    for vary in &entry.key.vary {
        payload.bytes(vary.name.as_str().as_bytes())?;
        payload.boolean(vary.present);
        payload.bytes(&vary.value)?;
    }
    payload.u16(entry.status.as_u16());
    payload.u32_len(entry.headers.len())?;
    for (name, value) in &entry.headers {
        payload.bytes(name.as_str().as_bytes())?;
        payload.bytes(value.as_bytes())?;
    }
    payload.bytes_u64(&entry.body)?;
    payload.u32_len(entry.tags.len())?;
    for tag in &entry.tags {
        payload.bytes(tag)?;
    }
    payload.duration(entry.policy.freshness_lifetime);
    payload.duration(entry.policy.stale_while_revalidate);
    payload.duration(entry.policy.stale_if_error);
    payload.optional_duration(entry.policy.retention.keep());
    payload.boolean(entry.policy.retention.request_stale_allowed());
    payload.boolean(entry.policy.always_revalidate);
    payload.boolean(entry.policy.must_revalidate_stale);
    payload.boolean(entry.policy.allows_authorized_reuse);
    payload.optional_bytes(entry.validators.etag.as_ref().map(HeaderValue::as_bytes))?;
    payload.optional_bytes(
        entry
            .validators
            .last_modified
            .as_ref()
            .map(HeaderValue::as_bytes),
    )?;
    payload.u64(u64::try_from(entry.charge).map_err(|_| DiskCacheError::RecordTooLarge)?);

    let payload_len = u64::try_from(payload.0.len()).map_err(|_| DiskCacheError::RecordTooLarge)?;
    let checksum = crc32fast::hash(&payload.0);
    let mut record = Vec::with_capacity(HEADER_BYTES.saturating_add(payload.0.len()));
    record.extend_from_slice(MAGIC);
    record.extend_from_slice(&RECORD_VERSION.to_le_bytes());
    record.extend_from_slice(&payload_len.to_le_bytes());
    record.extend_from_slice(&checksum.to_le_bytes());
    record.extend_from_slice(&payload.0);
    Ok(record)
}

#[allow(clippy::too_many_lines)]
fn decode_record(
    record: &[u8],
    config: &DiskCacheConfig,
    now: MonoTime,
) -> Result<DecodedRecord, DiskCacheError> {
    if record.len() < HEADER_BYTES || &record[..8] != MAGIC {
        return Err(invalid_record());
    }
    let version = u16::from_le_bytes(record[8..10].try_into().map_err(|_| invalid_record())?);
    let payload_len = u64::from_le_bytes(record[10..18].try_into().map_err(|_| invalid_record())?);
    let checksum = u32::from_le_bytes(record[18..22].try_into().map_err(|_| invalid_record())?);
    if !matches!(version, 1 | RECORD_VERSION)
        || usize::try_from(payload_len).ok() != Some(record.len().saturating_sub(HEADER_BYTES))
        || crc32fast::hash(&record[HEADER_BYTES..]) != checksum
    {
        return Err(invalid_record());
    }
    let mut input = Decoder::new(&record[HEADER_BYTES..]);
    let sequence = input.u64()?;
    let access = input.u64()?;
    let stored_age = input.duration()?;
    let stored_wall = UNIX_EPOCH
        .checked_add(input.duration()?)
        .ok_or_else(invalid_record)?;
    let downtime = SystemTime::now()
        .duration_since(stored_wall)
        .unwrap_or_default();
    let corrected_initial_age = stored_age.saturating_add(downtime);

    let method_bytes = input.bytes(config.memory.max_key_bytes)?;
    let method = Method::from_bytes(method_bytes).map_err(|_| invalid_record())?;
    let scheme = input.string(config.memory.max_key_bytes)?;
    let authority = input.string(config.memory.max_key_bytes)?;
    let path = input.string(config.memory.max_key_bytes)?;
    let query = input.optional_string(config.memory.max_key_bytes)?;
    let empty = HeaderMap::new();
    let canonical = BaseKey::new(
        RequestKeyInput {
            method: &method,
            scheme: &scheme,
            authority: &authority,
            path: &path,
            query: query.as_deref(),
            headers: &empty,
        },
        config.memory.max_key_bytes,
    )
    .map_err(|_| invalid_record())?;
    if canonical.method != method
        || canonical.scheme != scheme
        || canonical.authority != authority
        || canonical.path != path
        || canonical.query != query
        || !canonical.is_get()
    {
        return Err(invalid_record());
    }
    let vary_count = input.count(config.memory.max_vary_fields)?;
    let mut vary = Vec::with_capacity(vary_count);
    for _ in 0..vary_count {
        let name = HeaderName::from_bytes(input.bytes(config.memory.max_header_bytes)?)
            .map_err(|_| invalid_record())?;
        let present = input.boolean()?;
        let value = Bytes::copy_from_slice(input.bytes(config.memory.max_key_bytes)?);
        vary.push(VaryValue {
            name,
            present,
            value,
        });
    }
    if !vary
        .windows(2)
        .all(|pair| pair[0].name.as_str() < pair[1].name.as_str())
    {
        return Err(invalid_record());
    }
    let key = CacheKey {
        base: canonical,
        vary,
    };
    if key.encoded_len() > config.memory.max_key_bytes {
        return Err(invalid_record());
    }
    let status = StatusCode::from_u16(input.u16()?).map_err(|_| invalid_record())?;
    let header_count = input.count(config.memory.max_header_fields)?;
    let mut headers = HeaderMap::new();
    let mut header_bytes = 0usize;
    for _ in 0..header_count {
        let name_bytes = input.bytes(config.memory.max_header_bytes)?;
        let value_bytes = input.bytes(config.memory.max_header_bytes)?;
        header_bytes = header_bytes
            .checked_add(name_bytes.len())
            .and_then(|size| size.checked_add(value_bytes.len()))
            .ok_or_else(invalid_record)?;
        if header_bytes > config.memory.max_header_bytes {
            return Err(invalid_record());
        }
        let name = HeaderName::from_bytes(name_bytes).map_err(|_| invalid_record())?;
        let value = HeaderValue::from_bytes(value_bytes).map_err(|_| invalid_record())?;
        headers.append(name, value);
    }
    let body = Bytes::copy_from_slice(input.bytes_u64(config.memory.max_body_bytes)?);
    let tag_count = input.count(config.memory.max_tags_per_entry)?;
    let mut tags = Vec::with_capacity(tag_count);
    for _ in 0..tag_count {
        let tag = Bytes::copy_from_slice(input.bytes(config.memory.max_tag_bytes)?);
        validate_tag(&tag, config.memory.max_tag_bytes)?;
        if tags.contains(&tag) {
            return Err(invalid_record());
        }
        tags.push(tag);
    }
    let freshness_lifetime = input.duration()?;
    let stale_while_revalidate = input.duration()?;
    let stale_if_error = input.duration()?;
    let (retention_after_freshness, request_stale_allowed) = if version >= 2 {
        (input.optional_duration()?, input.boolean()?)
    } else {
        (None, true)
    };
    let retention = match (retention_after_freshness, request_stale_allowed) {
        (None, true) => RetentionPolicy::Rfc,
        (Some(keep), false) => RetentionPolicy::Canonical { keep },
        _ => return Err(invalid_record()),
    };
    let policy = ResponsePolicy {
        freshness_lifetime,
        corrected_initial_age,
        stale_while_revalidate,
        stale_if_error,
        retention,
        always_revalidate: input.boolean()?,
        must_revalidate_stale: input.boolean()?,
        allows_authorized_reuse: input.boolean()?,
    };
    let validators = Validators {
        etag: input
            .optional_bytes(config.memory.max_header_bytes)?
            .map(HeaderValue::from_bytes)
            .transpose()
            .map_err(|_| invalid_record())?,
        last_modified: input
            .optional_bytes(config.memory.max_header_bytes)?
            .map(HeaderValue::from_bytes)
            .transpose()
            .map_err(|_| invalid_record())?,
    };
    let stored_charge = usize::try_from(input.u64()?).map_err(|_| invalid_record())?;
    if !input.finished() {
        return Err(invalid_record());
    }
    let charge = object_charge(&key, &headers, body.len(), &tags).ok_or_else(invalid_record)?;
    if charge != stored_charge
        || charge > config.memory.max_object_bytes
        || charge > config.memory.max_total_bytes
    {
        return Err(invalid_record());
    }
    Ok(DecodedRecord {
        entry: RecoveredEntry {
            key,
            status,
            headers,
            body,
            tags,
            policy,
            validators,
            response_received: now,
            charge,
        },
        access,
        sequence,
    })
}

#[derive(Default)]
struct Encoder(Vec<u8>);

impl Encoder {
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn boolean(&mut self, value: bool) {
        self.0.push(u8::from(value));
    }

    fn duration(&mut self, value: Duration) {
        self.u64(value.as_secs());
        self.u32(value.subsec_nanos());
    }

    fn optional_duration(&mut self, value: Option<Duration>) {
        self.boolean(value.is_some());
        if let Some(value) = value {
            self.duration(value);
        }
    }

    fn u32_len(&mut self, value: usize) -> Result<(), DiskCacheError> {
        self.u32(u32::try_from(value).map_err(|_| DiskCacheError::RecordTooLarge)?);
        Ok(())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), DiskCacheError> {
        self.u32_len(value.len())?;
        self.0.extend_from_slice(value);
        Ok(())
    }

    fn bytes_u64(&mut self, value: &[u8]) -> Result<(), DiskCacheError> {
        self.u64(u64::try_from(value.len()).map_err(|_| DiskCacheError::RecordTooLarge)?);
        self.0.extend_from_slice(value);
        Ok(())
    }

    fn optional_bytes(&mut self, value: Option<&[u8]>) -> Result<(), DiskCacheError> {
        self.boolean(value.is_some());
        if let Some(value) = value {
            self.bytes(value)?;
        }
        Ok(())
    }
}

struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    const fn new(remaining: &'a [u8]) -> Self {
        Self { remaining }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DiskCacheError> {
        if self.remaining.len() < length {
            return Err(invalid_record());
        }
        let (value, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, DiskCacheError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| invalid_record())?,
        ))
    }

    fn u32(&mut self) -> Result<u32, DiskCacheError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| invalid_record())?,
        ))
    }

    fn u64(&mut self) -> Result<u64, DiskCacheError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| invalid_record())?,
        ))
    }

    fn boolean(&mut self) -> Result<bool, DiskCacheError> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid_record()),
        }
    }

    fn duration(&mut self) -> Result<Duration, DiskCacheError> {
        let seconds = self.u64()?;
        let nanos = self.u32()?;
        if nanos >= 1_000_000_000 {
            return Err(invalid_record());
        }
        Ok(Duration::new(seconds, nanos))
    }

    fn optional_duration(&mut self) -> Result<Option<Duration>, DiskCacheError> {
        self.boolean()?.then(|| self.duration()).transpose()
    }

    fn count(&mut self, maximum: usize) -> Result<usize, DiskCacheError> {
        let count = usize::try_from(self.u32()?).map_err(|_| invalid_record())?;
        if count > maximum {
            return Err(invalid_record());
        }
        Ok(count)
    }

    fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], DiskCacheError> {
        let length = usize::try_from(self.u32()?).map_err(|_| invalid_record())?;
        if length > maximum {
            return Err(invalid_record());
        }
        self.take(length)
    }

    fn bytes_u64(&mut self, maximum: usize) -> Result<&'a [u8], DiskCacheError> {
        let length = usize::try_from(self.u64()?).map_err(|_| invalid_record())?;
        if length > maximum {
            return Err(invalid_record());
        }
        self.take(length)
    }

    fn string(&mut self, maximum: usize) -> Result<String, DiskCacheError> {
        std::str::from_utf8(self.bytes(maximum)?)
            .map(str::to_owned)
            .map_err(|_| invalid_record())
    }

    fn optional_bytes(&mut self, maximum: usize) -> Result<Option<&'a [u8]>, DiskCacheError> {
        if self.boolean()? {
            self.bytes(maximum).map(Some)
        } else {
            Ok(None)
        }
    }

    fn optional_string(&mut self, maximum: usize) -> Result<Option<String>, DiskCacheError> {
        self.optional_bytes(maximum)?
            .map(|value| {
                std::str::from_utf8(value)
                    .map(str::to_owned)
                    .map_err(|_| invalid_record())
            })
            .transpose()
    }

    const fn finished(&self) -> bool {
        self.remaining.is_empty()
    }
}

fn verify_named_identity(
    root: &OwnedFd,
    name: &str,
    descriptor: &rustix_fs::Stat,
    owner: u32,
) -> Result<(), DiskCacheError> {
    let linked = rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW).map_err(storage)?;
    if !secure_file(descriptor, owner)
        || descriptor.st_dev != linked.st_dev
        || descriptor.st_ino != linked.st_ino
    {
        return Err(DiskCacheError::UnsafeEntry);
    }
    Ok(())
}

fn remove_owned_name(
    root: &OwnedFd,
    name: &str,
    expected: Option<(u64, u64)>,
) -> Result<(), DiskCacheError> {
    let current = match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(Errno::NOENT) => return Ok(()),
        Err(source) => return Err(storage(source)),
    };
    if expected.is_some_and(|(device, inode)| current.st_dev != device || current.st_ino != inode) {
        return Err(DiskCacheError::UnsafeEntry);
    }
    rustix_fs::unlinkat(root, name, AtFlags::empty()).map_err(storage)
}

fn remove_namespace_entry(
    root: &OwnedFd,
    name: &str,
    metadata: &rustix_fs::Stat,
) -> Result<(), DiskCacheError> {
    if FileType::from_raw_mode(metadata.st_mode).is_dir() {
        return Err(DiskCacheError::UnsafeEntry);
    }
    remove_owned_name(root, name, Some((metadata.st_dev, metadata.st_ino)))
}

fn secure_file(metadata: &rustix_fs::Stat, owner: u32) -> bool {
    FileType::from_raw_mode(metadata.st_mode).is_file()
        && metadata.st_uid == owner
        && metadata.st_mode & 0o777 == FILE_MODE
        && metadata.st_nlink == 1
}

fn is_temp_name(name: &[u8]) -> bool {
    uuid_name(name, TEMP_SUFFIX.as_bytes())
}

fn is_record_name(name: &[u8]) -> bool {
    let Some(middle) = name
        .strip_prefix(RECORD_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(RECORD_SUFFIX.as_bytes()))
    else {
        return false;
    };
    middle.len() == 16 + 1 + 32
        && middle[16] == b'-'
        && middle[..16].iter().all(u8::is_ascii_hexdigit)
        && middle[17..].iter().all(u8::is_ascii_hexdigit)
}

fn uuid_name(name: &[u8], suffix: &[u8]) -> bool {
    let Some(token) = name
        .strip_prefix(TEMP_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(suffix))
    else {
        return false;
    };
    token.len() == 32 && token.iter().all(u8::is_ascii_hexdigit)
}

fn is_owner_probe(name: &[u8]) -> bool {
    name.strip_prefix(OWNER_PROBE_PREFIX.as_bytes())
        .is_some_and(|token| token.len() == 32 && token.iter().all(u8::is_ascii_hexdigit))
}

fn storage(source: impl Into<io::Error>) -> DiskCacheError {
    DiskCacheError::Storage(source.into())
}

fn invalid_record() -> DiskCacheError {
    DiskCacheError::Storage(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid persistent cache record",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DiskCacheConfig {
        let mut config = DiskCacheConfig::default();
        config.memory.max_entries = 4;
        config.memory.max_total_bytes = 32 * 1024;
        config.memory.max_object_bytes = 8 * 1024;
        config.memory.max_body_bytes = 4 * 1024;
        config.max_disk_bytes = 64 * 1024;
        config.max_disk_files = 4;
        config.max_record_bytes = 16 * 1024;
        config
    }

    fn request(headers: &HeaderMap) -> RequestKeyInput<'_> {
        RequestKeyInput {
            method: &Method::GET,
            scheme: "https",
            authority: "example.com",
            path: "/fault",
            query: None,
            headers,
        }
    }

    fn attempt_store(cache: &DiskCache, point: FaultPoint) -> Result<StoreOutcome, DiskCacheError> {
        let request_headers = HeaderMap::new();
        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        let now = cache.now();
        let entry = cache.prepare(
            request(&request_headers),
            StatusCode::OK,
            &response_headers,
            Bytes::from_static(b"fault"),
            ResponseTiming {
                request_started: now,
                response_received: now,
                response_received_wall: SystemTime::now(),
            },
            &[],
        )?;
        cache.inject(point);
        match cache.begin_fill(entry.key().base().clone())? {
            DiskFillJoin::Leader(leader) => leader.store(entry),
            DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("expected leader"),
        }
    }

    #[test]
    fn publication_faults_recover_to_a_complete_record_or_a_miss() {
        for point in [
            FaultPoint::AfterFileSync,
            FaultPoint::AfterPublish,
            FaultPoint::BeforeDirectorySync,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().join("cache");
            let cache = DiskCache::open(&root, test_config()).expect("cache");
            assert!(matches!(
                attempt_store(&cache, point),
                Err(DiskCacheError::Injected)
            ));
            drop(cache);

            let recovered = DiskCache::open(&root, test_config()).expect("recovery");
            assert!(recovered.stats().disk_entries <= 1);
            let names = std::fs::read_dir(&root)
                .expect("root")
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect::<Vec<_>>();
            assert!(!names.iter().any(|name| is_temp_name(name.as_bytes())));
            let headers = HeaderMap::new();
            match recovered.lookup(request(&headers)).expect("lookup") {
                Lookup::Hit { response, .. } => {
                    assert_eq!(response.body, Bytes::from_static(b"fault"));
                }
                Lookup::Miss { .. } => {}
                other => panic!("unexpected recovered state: {other:?}"),
            }
        }
    }

    #[test]
    fn post_memory_reconciliation_fault_closes_and_recovers_published_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cache");
        let cache = DiskCache::open(&root, test_config()).expect("cache");
        assert!(matches!(
            attempt_store(&cache, FaultPoint::BeforeMemoryReconciliation),
            Err(DiskCacheError::Injected)
        ));
        assert!(matches!(
            cache.lookup(request(&HeaderMap::new())),
            Err(DiskCacheError::Closed)
        ));
        drop(cache);

        let recovered = DiskCache::open(&root, test_config()).expect("recovery");
        assert!(matches!(
            recovered.lookup(request(&HeaderMap::new())),
            Ok(Lookup::Hit { response, .. })
                if response.body == Bytes::from_static(b"fault")
        ));
    }

    #[test]
    fn insertion_reconciliation_preserves_the_new_record_on_replacement_overlap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cache");
        let cache = DiskCache::open(&root, test_config()).expect("cache");
        let request_headers = HeaderMap::new();
        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        let now = cache.now();
        let entry = cache
            .prepare(
                request(&request_headers),
                StatusCode::OK,
                &response_headers,
                Bytes::from_static(b"record"),
                ResponseTiming {
                    request_started: now,
                    response_received: now,
                    response_received_wall: SystemTime::now(),
                },
                &[],
            )
            .expect("entry");
        let key = entry.key.clone();
        match cache.begin_fill(key.base().clone()).expect("fill") {
            DiskFillJoin::Leader(leader) => leader.store(entry).expect("store"),
            DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
        };

        let mut state = cache.shared.lock();
        let delta = InsertionDelta {
            inserted: key.clone(),
            removed: vec![key.clone()],
            evicted: 0,
        };
        assert_eq!(
            reconcile_insertion(&cache.shared.root, &mut state, &delta).expect("reconcile"),
            0
        );
        assert!(state.records.contains_key(&key));
    }

    #[test]
    fn recovered_entries_receive_the_current_cache_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cache");
        let cache = DiskCache::open(&root, test_config()).expect("cache");
        let request_headers = HeaderMap::new();
        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        let now = cache.now();
        let entry = cache
            .prepare(
                request(&request_headers),
                StatusCode::OK,
                &response_headers,
                Bytes::from_static(b"recovered"),
                ResponseTiming {
                    request_started: now,
                    response_received: now,
                    response_received_wall: SystemTime::now(),
                },
                &[],
            )
            .expect("entry");
        let key = entry.key().clone();
        match cache.begin_fill(key.base().clone()).expect("fill") {
            DiskFillJoin::Leader(leader) => {
                leader.store(entry).expect("initial store");
            }
            DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
        }
        drop(cache);

        let recovered = DiskCache::open(&root, test_config()).expect("recovery");
        let entry = recovered.shared.cache.entry(&key).expect("recovered entry");
        match recovered.begin_fill(key.base().clone()).expect("fill") {
            DiskFillJoin::Leader(leader) => assert!(matches!(
                leader.store(entry),
                Ok(StoreOutcome::Stored { .. })
            )),
            DiskFillJoin::Follower(_) | DiskFillJoin::AtCapacity => panic!("leader"),
        }
    }
}
