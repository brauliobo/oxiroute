#[allow(clippy::wildcard_imports)]
use super::*;

/// Shared HTTP cache whose accepted representations and LRU order survive process restart.
#[derive(Clone)]
pub struct DiskCache {
    pub(super) shared: Arc<Shared>,
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

impl DiskCache {
    /// Validates persistent cache limits and root access without creating files, acquiring the
    /// ownership lease, cleaning records, or retaining descriptors.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid limits, unsafe path components, inaccessible parents, or an
    /// existing root that is not a writable mode-0700 directory.
    pub fn validate(
        root: impl AsRef<Path>,
        config: &DiskCacheConfig,
    ) -> Result<(), DiskCacheError> {
        validate_disk_config(config)?;
        validate_root_access(root.as_ref())
    }

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
    pub(super) fn inject(&self, point: FaultPoint) {
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
