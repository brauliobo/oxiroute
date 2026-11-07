#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct Shared {
    pub(super) cache: Cache,
    pub(super) config: DiskCacheConfig,
    pub(super) root: OwnedFd,
    pub(super) root_owner: u32,
    pub(super) _ownership: OwnedFd,
    pub(super) state: Mutex<State>,
    pub(super) closed: AtomicBool,
    #[cfg(test)]
    pub(super) fault: Mutex<Option<FaultPoint>>,
}

pub(super) struct State {
    pub(super) records: HashMap<CacheKey, RecordInfo>,
    pub(super) tags: HashMap<Bytes, BTreeSet<CacheKey>>,
    pub(super) bytes_used: u64,
    pub(super) sequence: u64,
    pub(super) recovered: u64,
    pub(super) stale_temps_removed: u64,
    pub(super) corrupt_records_removed: u64,
}

#[derive(Clone)]
pub(super) struct RecordInfo {
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) access: u64,
    pub(super) sequence: u64,
    pub(super) tags: Vec<Bytes>,
}

pub(super) struct DecodedRecord {
    pub(super) entry: RecoveredEntry,
    pub(super) access: u64,
    pub(super) sequence: u64,
}

impl Shared {
    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn touch(&self, state: &mut State, key: &CacheKey) -> Result<(), DiskCacheError> {
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

    pub(super) fn reconcile_base(
        &self,
        state: &mut State,
        base: &BaseKey,
    ) -> Result<(), DiskCacheError> {
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

    pub(super) fn publish(
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

    pub(super) fn remove_record(&self, record: &RecordInfo) -> Result<(), DiskCacheError> {
        remove_owned_name(
            &self.root,
            &record.name,
            Some((record.device, record.inode)),
        )
    }

    pub(super) fn sync_root(&self) -> Result<(), DiskCacheError> {
        self.durability(rustix_fs::fsync(&self.root).map_err(storage))
    }

    pub(super) fn durability<T>(
        &self,
        result: Result<T, DiskCacheError>,
    ) -> Result<T, DiskCacheError> {
        if result.is_err() {
            self.closed.store(true, Ordering::Release);
        }
        result
    }

    #[cfg(test)]
    pub(super) fn fault(&self, point: FaultPoint) -> Result<(), DiskCacheError> {
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
    pub(super) fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    pub(super) fn insert(&mut self, key: CacheKey, record: RecordInfo) {
        self.bytes_used = self.bytes_used.saturating_add(record.size);
        for tag in &record.tags {
            self.tags
                .entry(tag.clone())
                .or_default()
                .insert(key.clone());
        }
        self.records.insert(key, record);
    }

    pub(super) fn remove(&mut self, key: &CacheKey) -> Option<RecordInfo> {
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

pub(super) fn admission_victims(
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
