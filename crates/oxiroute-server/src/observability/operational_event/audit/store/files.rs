#[allow(clippy::wildcard_imports)]
use super::*;

/// What a scan of the audit files found.
#[derive(Default)]
pub(super) struct Loaded {
    pub(super) records: Vec<AuditRecord>,
    pub(super) max_sequence: u64,
    pub(super) corrupt: u64,
}

/// The files of a persistent audit store; sizes are tracked, so a write never rescans anything.
///
/// Index 0 is the active file and indexes `1..=max_rotated_files` are the rotated files, newest first.
pub(super) struct AuditFiles {
    root: OwnedFd,
    active: Option<File>,
    sizes: Vec<u64>,
    first_ids: Vec<u64>,
    directory_dirty: bool,
    #[cfg(test)]
    syncs: u64,
}

impl AuditFiles {
    pub(super) fn load(
        root: OwnedFd,
        limits: &AuditLimits,
    ) -> Result<(Self, Loaded), AuditStoreError> {
        let count = limits.max_rotated_files + 1;
        let mut files = Self {
            root,
            active: None,
            sizes: vec![0; count],
            first_ids: vec![0; count],
            directory_dirty: false,
            #[cfg(test)]
            syncs: 0,
        };
        let mut loaded = Loaded::default();
        for index in (0..count).rev() {
            let name = file_name(index);
            let before = loaded.records.len();
            scan_audit_file(&files.root, &name, &mut loaded)?;
            files.sizes[index] = file_size(&files.root, &name)?;
            files.first_ids[index] = loaded.records.get(before).map_or(0, |record| record.id);
        }
        Ok((files, loaded))
    }

    pub(super) fn total_bytes(&self) -> u64 {
        self.sizes.iter().sum()
    }

    pub(super) fn rotated_files(&self) -> u64 {
        self.sizes[1..]
            .iter()
            .map(|size| u64::from(*size > 0))
            .sum()
    }

    /// The oldest record that is still on disk.
    pub(super) fn oldest_id(&self) -> Option<u64> {
        self.first_ids.iter().rev().copied().find(|id| *id > 0)
    }

    #[cfg(test)]
    pub(super) const fn syncs(&self) -> u64 {
        self.syncs
    }

    /// Appends the lines in order, rotating by size, then makes them durable with a single sync.
    pub(super) fn write_batch(
        &mut self,
        batch: &[PendingLine],
        limits: &AuditLimits,
    ) -> Result<(), AuditStoreError> {
        let result = self
            .write_lines(batch, limits)
            .and_then(|()| self.prune(limits));
        if result.is_err() {
            self.resync(limits);
        }
        result
    }

    fn write_lines(
        &mut self,
        batch: &[PendingLine],
        limits: &AuditLimits,
    ) -> Result<(), AuditStoreError> {
        let mut run = Vec::new();
        for line in batch {
            let length = line.bytes.len() as u64;
            let size = self.sizes[0] + run.len() as u64;
            if size > 0 && size + length > limits.max_file_bytes {
                self.write_run(&mut run)?;
                self.rotate(limits)?;
            }
            if self.first_ids[0] == 0 {
                self.first_ids[0] = line.id;
            }
            run.extend_from_slice(&line.bytes);
        }
        self.write_run(&mut run)?;
        self.sync_active()?;
        self.sync_directory()
    }

    fn write_run(&mut self, run: &mut Vec<u8>) -> Result<(), AuditStoreError> {
        if run.is_empty() {
            return Ok(());
        }
        self.active_file()?
            .write_all(run)
            .map_err(|_| AuditStoreError::Io)?;
        self.sizes[0] += run.len() as u64;
        run.clear();
        Ok(())
    }

    fn active_file(&mut self) -> Result<&mut File, AuditStoreError> {
        if self.active.is_none() {
            let descriptor = open_audit_file(
                &self.root,
                AUDIT_ACTIVE_FILE,
                OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
            )?;
            self.directory_dirty |= self.sizes[0] == 0;
            self.active = Some(File::from(descriptor));
        }
        Ok(self
            .active
            .as_mut()
            .expect("active audit file was just opened"))
    }

    fn sync_active(&mut self) -> Result<(), AuditStoreError> {
        #[cfg(test)]
        {
            self.syncs += 1;
        }
        self.active_file()?
            .sync_data()
            .map_err(|_| AuditStoreError::Io)
    }

    fn sync_directory(&mut self) -> Result<(), AuditStoreError> {
        if std::mem::take(&mut self.directory_dirty) {
            rustix_fs::fsync(&self.root).map_err(|_| AuditStoreError::Io)?;
        }
        Ok(())
    }

    fn rotate(&mut self, limits: &AuditLimits) -> Result<(), AuditStoreError> {
        self.sync_active()?;
        self.active = None;
        for index in (1..=limits.max_rotated_files).rev() {
            let destination = rotated_name(index);
            ensure_safe_entry(&self.root, &destination)?;
            match rustix_fs::renameat_with(
                &self.root,
                file_name(index - 1).as_str(),
                &self.root,
                destination.as_str(),
                RenameFlags::empty(),
            ) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(_) => return Err(AuditStoreError::Io),
            }
        }
        self.sizes.rotate_right(1);
        self.first_ids.rotate_right(1);
        self.sizes[0] = 0;
        self.first_ids[0] = 0;
        self.directory_dirty = true;
        Ok(())
    }

    fn prune(&mut self, limits: &AuditLimits) -> Result<(), AuditStoreError> {
        for index in (1..=limits.max_rotated_files).rev() {
            if self.total_bytes() <= limits.max_total_bytes {
                break;
            }
            if self.sizes[index] == 0 {
                continue;
            }
            let name = rotated_name(index);
            ensure_safe_entry(&self.root, &name)?;
            rustix_fs::unlinkat(&self.root, name.as_str(), AtFlags::empty())
                .map_err(|_| AuditStoreError::Io)?;
            self.sizes[index] = 0;
            self.first_ids[index] = 0;
            self.directory_dirty = true;
        }
        self.sync_directory()
    }

    /// Reads the sizes back after a failed write, so the counters match the disk again.
    fn resync(&mut self, limits: &AuditLimits) {
        self.active = None;
        for index in 0..=limits.max_rotated_files {
            self.sizes[index] = file_size(&self.root, &file_name(index)).unwrap_or(0);
        }
    }

    /// Rewrites the store as exactly `records`; used when opening a store that holds more than its cap.
    pub(super) fn compact(
        &mut self,
        records: &[AuditRecord],
        limits: &AuditLimits,
    ) -> Result<(), AuditStoreError> {
        ensure_safe_entry(&self.root, AUDIT_ACTIVE_FILE)?;
        ensure_safe_entry(&self.root, AUDIT_TEMP_FILE)?;
        match rustix_fs::unlinkat(&self.root, AUDIT_TEMP_FILE, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(_) => return Err(AuditStoreError::Io),
        }
        let descriptor = rustix_fs::openat(
            &self.root,
            AUDIT_TEMP_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(AUDIT_FILE_MODE),
        )
        .map_err(|_| AuditStoreError::Io)?;
        let mut file = File::from(descriptor);
        let mut written = 0_u64;
        for record in records {
            let line = encode_line(record, limits.max_record_bytes)?;
            file.write_all(&line).map_err(|_| AuditStoreError::Io)?;
            written += line.len() as u64;
        }
        file.sync_all().map_err(|_| AuditStoreError::Io)?;
        ensure_safe_entry(&self.root, AUDIT_ACTIVE_FILE)?;
        rustix_fs::renameat_with(
            &self.root,
            AUDIT_TEMP_FILE,
            &self.root,
            AUDIT_ACTIVE_FILE,
            RenameFlags::empty(),
        )
        .map_err(|_| AuditStoreError::Io)?;
        for index in 1..=limits.max_rotated_files {
            let name = rotated_name(index);
            ensure_safe_entry(&self.root, &name)?;
            if self.sizes[index] > 0 {
                rustix_fs::unlinkat(&self.root, name.as_str(), AtFlags::empty())
                    .map_err(|_| AuditStoreError::Io)?;
            }
        }
        self.active = None;
        self.sizes.fill(0);
        self.first_ids.fill(0);
        self.sizes[0] = written;
        self.first_ids[0] = records.first().map_or(0, |record| record.id);
        rustix_fs::fsync(&self.root).map_err(|_| AuditStoreError::Io)
    }
}

/// One JSON line, bounded by the record limits.
pub(super) fn encode_line(
    record: &AuditRecord,
    max_record_bytes: usize,
) -> Result<Vec<u8>, AuditStoreError> {
    let mut bytes = serde_json::to_vec(record).map_err(|_| AuditStoreError::Io)?;
    bytes.push(b'\n');
    if bytes.len() > max_record_bytes {
        return Err(AuditStoreError::RecordTooLarge);
    }
    Ok(bytes)
}

pub(super) fn open_or_create_root(path: &Path) -> Result<OwnedFd, AuditStoreError> {
    if path.as_os_str().is_empty() {
        return Err(AuditStoreError::RootOpen);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let anchor = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut directory =
        rustix_fs::open(anchor, flags, Mode::empty()).map_err(|_| AuditStoreError::RootOpen)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir | Component::CurDir) {
                continue;
            }
            return Err(AuditStoreError::RootOpen);
        };
        directory = match rustix_fs::openat(&directory, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) => {
                rustix_fs::mkdirat(&directory, name, Mode::from_raw_mode(AUDIT_ROOT_MODE))
                    .map_err(|_| AuditStoreError::RootOpen)?;
                let next = rustix_fs::openat(&directory, name, flags, Mode::empty())
                    .map_err(|_| AuditStoreError::RootOpen)?;
                rustix_fs::fchmod(&next, Mode::from_raw_mode(AUDIT_ROOT_MODE))
                    .map_err(|_| AuditStoreError::RootOpen)?;
                rustix_fs::fsync(&directory).map_err(|_| AuditStoreError::RootOpen)?;
                next
            }
            Err(_) => return Err(AuditStoreError::RootOpen),
        };
    }
    let metadata = rustix_fs::fstat(&directory).map_err(|_| AuditStoreError::RootOpen)?;
    if !FileType::from_raw_mode(metadata.st_mode).is_dir()
        || metadata.st_mode & 0o7777 != AUDIT_ROOT_MODE
    {
        return Err(AuditStoreError::RootOpen);
    }
    Ok(directory)
}

fn open_audit_file(root: &OwnedFd, name: &str, flags: OFlags) -> Result<OwnedFd, AuditStoreError> {
    let descriptor = rustix_fs::openat(
        root,
        name,
        flags | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(AUDIT_FILE_MODE),
    )
    .map_err(|_| AuditStoreError::Io)?;
    let metadata = rustix_fs::fstat(&descriptor).map_err(|_| AuditStoreError::Io)?;
    if !FileType::from_raw_mode(metadata.st_mode).is_file()
        || metadata.st_mode & 0o7777 != AUDIT_FILE_MODE
    {
        return Err(AuditStoreError::UnsafeEntry);
    }
    Ok(descriptor)
}

fn ensure_safe_entry(root: &OwnedFd, name: &str) -> Result<(), AuditStoreError> {
    match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata)
            if FileType::from_raw_mode(metadata.st_mode).is_file()
                && metadata.st_mode & 0o7777 == AUDIT_FILE_MODE =>
        {
            Ok(())
        }
        Ok(_) => Err(AuditStoreError::UnsafeEntry),
        Err(Errno::NOENT) => Ok(()),
        Err(_) => Err(AuditStoreError::Io),
    }
}

fn file_size(root: &OwnedFd, name: &str) -> Result<u64, AuditStoreError> {
    match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => {
            if !FileType::from_raw_mode(metadata.st_mode).is_file()
                || metadata.st_mode & 0o7777 != AUDIT_FILE_MODE
            {
                return Err(AuditStoreError::UnsafeEntry);
            }
            u64::try_from(metadata.st_size).map_err(|_| AuditStoreError::Io)
        }
        Err(Errno::NOENT) => Ok(0),
        Err(_) => Err(AuditStoreError::Io),
    }
}

fn scan_audit_file(root: &OwnedFd, name: &str, loaded: &mut Loaded) -> Result<(), AuditStoreError> {
    if file_size(root, name)? == 0 {
        return Ok(());
    }
    let descriptor = open_audit_file(root, name, OFlags::RDONLY)?;
    let mut bytes = Vec::new();
    File::from(descriptor)
        .take(u64::try_from(AUDIT_SCAN_LIMIT_BYTES).expect("audit scan limit fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|_| AuditStoreError::Io)?;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(mut record) = serde_json::from_slice::<AuditRecord>(line) else {
            loaded.corrupt = loaded.corrupt.saturating_add(1);
            continue;
        };
        if record.id == 0
            || record.correlation_id.len() > CORRELATION_ID_MAX_BYTES
            || record.operation.is_empty()
        {
            loaded.corrupt = loaded.corrupt.saturating_add(1);
            continue;
        }
        record.correlation_id = redact_identifier(&record.correlation_id);
        record.actor = redact_identifier(&record.actor);
        record.source = redact_identifier(&record.source);
        record.operation = redact_identifier(&record.operation);
        record.revision = record.revision.map(|revision| redact_text(&revision));
        loaded.max_sequence = loaded.max_sequence.max(record.id);
        loaded.records.push(record);
    }
    Ok(())
}

fn file_name(index: usize) -> String {
    if index == 0 {
        AUDIT_ACTIVE_FILE.to_owned()
    } else {
        rotated_name(index)
    }
}

fn rotated_name(index: usize) -> String {
    format!("{AUDIT_ACTIVE_FILE}.{index}")
}
