#[allow(clippy::wildcard_imports)]
use super::*;

struct AuditState {
    records: VecDeque<AuditRecord>,
    next_sequence: u64,
    bytes: u64,
    rotated_files: u64,
    write_failures: u64,
    corrupt_records: u64,
    last_error: Option<&'static str>,
    degraded: bool,
    // Prometheus `_total` families must not fall when retained records are evicted.
    operation_counts: [[u64; 7]; 4],
    operation_counts_initialized: bool,
}

impl AuditState {
    pub(super) fn new() -> Self {
        Self {
            records: VecDeque::new(),
            next_sequence: 0,
            bytes: 0,
            rotated_files: 0,
            write_failures: 0,
            corrupt_records: 0,
            last_error: None,
            degraded: false,
            operation_counts: [[0; 7]; 4],
            operation_counts_initialized: false,
        }
    }
}

pub(crate) struct AuditStore {
    root: Option<OwnedFd>,
    limits: AuditLimits,
    state: Mutex<AuditState>,
}

impl AuditStore {
    pub(crate) fn memory(limits: AuditLimits) -> Self {
        Self {
            root: None,
            limits,
            state: Mutex::new(AuditState::new()),
        }
    }

    pub(crate) fn degraded(limits: AuditLimits, error: &'static str) -> Self {
        let mut state = AuditState::new();
        state.degraded = true;
        state.last_error = Some(error);
        Self {
            root: None,
            limits,
            state: Mutex::new(state),
        }
    }

    pub(crate) fn open(path: &Path, limits: AuditLimits) -> Result<Self, AuditStoreError> {
        if !limits.valid() {
            return Err(AuditStoreError::InvalidLimits);
        }
        let root = open_or_create_root(path)?;
        let store = Self {
            root: Some(root),
            limits,
            state: Mutex::new(AuditState::new()),
        };
        {
            let mut state = store
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.load_locked(&mut state)?;
            if state.records.len() > limits.max_records {
                let remove = state.records.len().saturating_sub(limits.max_records);
                for _ in 0..remove {
                    state.records.pop_front();
                }
                let records = state.records.iter().cloned().collect::<Vec<_>>();
                store.compact_locked(&records)?;
                store.load_locked(&mut state)?;
            }
        }
        Ok(store)
    }

    pub(crate) fn append(
        &self,
        context: &AuditContext,
        category: AuditCategory,
        operation: &str,
        result: AuditResult,
        revision: Option<&EffectiveRevision>,
    ) -> Result<u64, AuditStoreError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_sequence.saturating_add(1);
        let record = AuditRecord {
            id,
            timestamp_unix_ms: unix_time_ms().unwrap_or(0),
            correlation_id: redact_identifier(&context.correlation_id),
            actor: redact_identifier(&context.actor),
            source: redact_identifier(&context.source),
            category,
            operation: redact_identifier(operation),
            result,
            revision: revision.map(|revision| revision.as_str().to_owned()),
        };
        let append_result = self.append_locked(&mut state, &record);
        match append_result {
            Ok(()) => {
                state.next_sequence = id;
                if self.root.is_none() {
                    state.records.push_back(record);
                    increment_operation_count(&mut state.operation_counts, category, result);
                    while state.records.len() > self.limits.max_records
                        || state.bytes > self.limits.max_total_bytes
                    {
                        let Some(oldest) = state.records.pop_front() else {
                            break;
                        };
                        let bytes = serde_json::to_vec(&oldest).map_or(0, |value| {
                            u64::try_from(value.len().saturating_add(1)).unwrap_or(u64::MAX)
                        });
                        state.bytes = state.bytes.saturating_sub(bytes);
                    }
                } else {
                    increment_operation_count(&mut state.operation_counts, category, result);
                    while state.records.len() > self.limits.max_records {
                        state.records.pop_front();
                    }
                }
                if state.corrupt_records == 0 && (self.root.is_some() || !state.degraded) {
                    state.last_error = None;
                    state.degraded = false;
                }
                Ok(id)
            }
            Err(error) => {
                state.write_failures = state.write_failures.saturating_add(1);
                state.last_error = Some(error.code());
                state.degraded = self.root.is_some() || state.degraded;
                Err(error)
            }
        }
    }

    pub(crate) fn page(
        &self,
        after: u64,
        limit: usize,
        category: Option<AuditCategory>,
        result: Option<AuditResult>,
    ) -> AuditPage {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let records: Vec<_> = state
            .records
            .iter()
            .filter(|record| {
                record.id > after
                    && category.is_none_or(|category| record.category == category)
                    && result.is_none_or(|result| record.result == result)
            })
            .take(limit)
            .cloned()
            .collect();
        let cursor = records.last().map_or(after, |record| record.id);
        let has_more = state.records.iter().any(|record| {
            record.id > cursor
                && category.is_none_or(|category| record.category == category)
                && result.is_none_or(|result| record.result == result)
        });
        AuditPage {
            records,
            cursor,
            has_more,
            oldest_cursor: state.records.front().map(|record| record.id),
            latest_cursor: state.next_sequence,
        }
    }

    pub(crate) fn status(&self) -> AuditStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AuditStatus {
            state: if state.degraded {
                AuditComponentState::Degraded
            } else if self.root.is_some() {
                AuditComponentState::Healthy
            } else {
                AuditComponentState::Memory
            },
            persistent: self.root.is_some(),
            degraded: state.degraded,
            record_count: u64::try_from(state.records.len()).unwrap_or(u64::MAX),
            bytes: state.bytes,
            rotated_files: state.rotated_files,
            max_records: u64::try_from(self.limits.max_records).unwrap_or(u64::MAX),
            max_record_bytes: u64::try_from(self.limits.max_record_bytes).unwrap_or(u64::MAX),
            max_file_bytes: self.limits.max_file_bytes,
            max_total_bytes: self.limits.max_total_bytes,
            max_rotated_files: u64::try_from(self.limits.max_rotated_files).unwrap_or(u64::MAX),
            write_failures: state.write_failures,
            corrupt_records: state.corrupt_records,
            last_error: state.last_error,
        }
    }

    pub(crate) fn metric_snapshot(&self) -> AuditMetricSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AuditMetricSnapshot {
            status: self.status_locked(&state),
            operation_counts: state.operation_counts,
        }
    }

    fn status_locked(&self, state: &AuditState) -> AuditStatus {
        AuditStatus {
            state: if state.degraded {
                AuditComponentState::Degraded
            } else if self.root.is_some() {
                AuditComponentState::Healthy
            } else {
                AuditComponentState::Memory
            },
            persistent: self.root.is_some(),
            degraded: state.degraded,
            record_count: u64::try_from(state.records.len()).unwrap_or(u64::MAX),
            bytes: state.bytes,
            rotated_files: state.rotated_files,
            max_records: u64::try_from(self.limits.max_records).unwrap_or(u64::MAX),
            max_record_bytes: u64::try_from(self.limits.max_record_bytes).unwrap_or(u64::MAX),
            max_file_bytes: self.limits.max_file_bytes,
            max_total_bytes: self.limits.max_total_bytes,
            max_rotated_files: u64::try_from(self.limits.max_rotated_files).unwrap_or(u64::MAX),
            write_failures: state.write_failures,
            corrupt_records: state.corrupt_records,
            last_error: state.last_error,
        }
    }

    fn append_locked(
        &self,
        state: &mut AuditState,
        record: &AuditRecord,
    ) -> Result<(), AuditStoreError> {
        let bytes = serde_json::to_vec(record).map_err(|_| AuditStoreError::Io)?;
        let record_bytes = bytes
            .len()
            .checked_add(1)
            .ok_or(AuditStoreError::RecordTooLarge)?;
        if record_bytes > self.limits.max_record_bytes
            || u64::try_from(record_bytes).is_ok_and(|bytes| bytes > self.limits.max_file_bytes)
        {
            return Err(AuditStoreError::RecordTooLarge);
        }
        let Some(root) = &self.root else {
            state.bytes = state
                .bytes
                .saturating_add(u64::try_from(record_bytes).unwrap_or(u64::MAX));
            return Ok(());
        };
        let active_size = file_size(root, AUDIT_ACTIVE_FILE)?;
        let record_size =
            u64::try_from(record_bytes).map_err(|_| AuditStoreError::RecordTooLarge)?;
        if active_size > 0
            && active_size
                .checked_add(record_size)
                .is_none_or(|size| size > self.limits.max_file_bytes)
        {
            self.rotate(root)?;
        }
        let descriptor = open_audit_file(
            root,
            AUDIT_ACTIVE_FILE,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
        )?;
        let mut file = File::from(descriptor);
        file.write_all(&bytes).map_err(|_| AuditStoreError::Io)?;
        file.write_all(b"\n").map_err(|_| AuditStoreError::Io)?;
        file.sync_all().map_err(|_| AuditStoreError::Io)?;
        rustix_fs::fsync(root).map_err(|_| AuditStoreError::Io)?;
        self.prune_total_bytes(root)?;
        self.load_locked(state)?;
        if state.records.len() > self.limits.max_records {
            let remove = state.records.len().saturating_sub(self.limits.max_records);
            for _ in 0..remove {
                state.records.pop_front();
            }
            let records = state.records.iter().cloned().collect::<Vec<_>>();
            self.compact_locked(&records)?;
            self.load_locked(state)?;
        }
        Ok(())
    }

    fn load_locked(&self, state: &mut AuditState) -> Result<(), AuditStoreError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let mut records = Vec::new();
        let mut max_sequence = state.next_sequence;
        let mut corrupt = 0_u64;
        for index in (1..=self.limits.max_rotated_files).rev() {
            let name = rotated_name(index);
            scan_audit_file(root, &name, &mut records, &mut max_sequence, &mut corrupt)?;
        }
        scan_audit_file(
            root,
            AUDIT_ACTIVE_FILE,
            &mut records,
            &mut max_sequence,
            &mut corrupt,
        )?;
        state.records = records.into_iter().collect();
        state.next_sequence = max_sequence;
        state.corrupt_records = state.corrupt_records.saturating_add(corrupt);
        let mut retained_operation_counts = [[0; 7]; 4];
        for record in &state.records {
            increment_operation_count(
                &mut retained_operation_counts,
                record.category,
                record.result,
            );
        }
        if !state.operation_counts_initialized {
            state.operation_counts = retained_operation_counts;
            state.operation_counts_initialized = true;
        }
        state.bytes = total_bytes(root, self.limits.max_rotated_files)?;
        state.rotated_files = rotated_file_count(root, self.limits.max_rotated_files)?;
        if corrupt > 0 {
            state.last_error = Some("corrupt_record_skipped");
            state.degraded = true;
        }
        Ok(())
    }

    fn rotate(&self, root: &OwnedFd) -> Result<(), AuditStoreError> {
        if self.limits.max_rotated_files == 0 {
            return Err(AuditStoreError::RecordTooLarge);
        }
        for index in (1..=self.limits.max_rotated_files).rev() {
            let source = if index == 1 {
                AUDIT_ACTIVE_FILE.to_owned()
            } else {
                rotated_name(index - 1)
            };
            let destination = rotated_name(index);
            ensure_safe_entry(root, &destination)?;
            match rustix_fs::renameat_with(
                root,
                source.as_str(),
                root,
                destination.as_str(),
                RenameFlags::empty(),
            ) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(_) => return Err(AuditStoreError::Io),
            }
        }
        rustix_fs::fsync(root).map_err(|_| AuditStoreError::Io)
    }

    fn prune_total_bytes(&self, root: &OwnedFd) -> Result<(), AuditStoreError> {
        let mut total = total_bytes(root, self.limits.max_rotated_files)?;
        for index in (1..=self.limits.max_rotated_files).rev() {
            if total <= self.limits.max_total_bytes {
                break;
            }
            let name = rotated_name(index);
            let size = file_size(root, &name)?;
            if size == 0 {
                continue;
            }
            ensure_safe_entry(root, &name)?;
            rustix_fs::unlinkat(root, name.as_str(), AtFlags::empty())
                .map_err(|_| AuditStoreError::Io)?;
            total = total.saturating_sub(size);
        }
        rustix_fs::fsync(root).map_err(|_| AuditStoreError::Io)
    }

    fn compact_locked(&self, records: &[AuditRecord]) -> Result<(), AuditStoreError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        ensure_safe_entry(root, AUDIT_ACTIVE_FILE)?;
        ensure_safe_entry(root, AUDIT_TEMP_FILE)?;
        match rustix_fs::unlinkat(root, AUDIT_TEMP_FILE, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(_) => return Err(AuditStoreError::Io),
        }
        let descriptor = rustix_fs::openat(
            root,
            AUDIT_TEMP_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(AUDIT_FILE_MODE),
        )
        .map_err(|_| AuditStoreError::Io)?;
        let mut file = File::from(descriptor);
        for record in records {
            let bytes = serde_json::to_vec(record).map_err(|_| AuditStoreError::Io)?;
            if bytes.len().saturating_add(1) > self.limits.max_record_bytes {
                return Err(AuditStoreError::RecordTooLarge);
            }
            file.write_all(&bytes).map_err(|_| AuditStoreError::Io)?;
            file.write_all(b"\n").map_err(|_| AuditStoreError::Io)?;
        }
        file.sync_all().map_err(|_| AuditStoreError::Io)?;
        ensure_safe_entry(root, AUDIT_ACTIVE_FILE)?;
        rustix_fs::renameat_with(
            root,
            AUDIT_TEMP_FILE,
            root,
            AUDIT_ACTIVE_FILE,
            RenameFlags::empty(),
        )
        .map_err(|_| AuditStoreError::Io)?;
        for index in 1..=self.limits.max_rotated_files {
            let name = rotated_name(index);
            ensure_safe_entry(root, &name)?;
            if file_size(root, &name)? > 0 {
                rustix_fs::unlinkat(root, name.as_str(), AtFlags::empty())
                    .map_err(|_| AuditStoreError::Io)?;
            }
        }
        rustix_fs::fsync(root).map_err(|_| AuditStoreError::Io)
    }
}

pub(super) fn environment_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub(super) fn environment_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn open_or_create_root(path: &Path) -> Result<OwnedFd, AuditStoreError> {
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

fn total_bytes(root: &OwnedFd, max_rotated_files: usize) -> Result<u64, AuditStoreError> {
    let mut total = file_size(root, AUDIT_ACTIVE_FILE)?;
    for index in 1..=max_rotated_files {
        total = total
            .checked_add(file_size(root, &rotated_name(index))?)
            .ok_or(AuditStoreError::Io)?;
    }
    Ok(total)
}

fn rotated_file_count(root: &OwnedFd, max_rotated_files: usize) -> Result<u64, AuditStoreError> {
    let mut count = 0_u64;
    for index in 1..=max_rotated_files {
        if file_size(root, &rotated_name(index))? > 0 {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn scan_audit_file(
    root: &OwnedFd,
    name: &str,
    records: &mut Vec<AuditRecord>,
    max_sequence: &mut u64,
    corrupt: &mut u64,
) -> Result<(), AuditStoreError> {
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
            *corrupt = corrupt.saturating_add(1);
            continue;
        };
        if record.id == 0
            || record.correlation_id.len() > CORRELATION_ID_MAX_BYTES
            || record.operation.is_empty()
        {
            *corrupt = corrupt.saturating_add(1);
            continue;
        }
        record.correlation_id = redact_identifier(&record.correlation_id);
        record.actor = redact_identifier(&record.actor);
        record.source = redact_identifier(&record.source);
        record.operation = redact_identifier(&record.operation);
        record.revision = record.revision.map(|revision| redact_text(&revision));
        *max_sequence = (*max_sequence).max(record.id);
        records.push(record);
    }
    Ok(())
}

fn rotated_name(index: usize) -> String {
    format!("{AUDIT_ACTIVE_FILE}.{index}")
}

fn increment_operation_count(
    counts: &mut [[u64; 7]; 4],
    category: AuditCategory,
    result: AuditResult,
) {
    let count = &mut counts[category.index()][result.index()];
    *count = count.saturating_add(1);
}
