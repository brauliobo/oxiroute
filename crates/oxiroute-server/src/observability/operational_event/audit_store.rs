#[allow(clippy::wildcard_imports)]
use super::*;

mod files;
mod writer;

use files::{AuditFiles, encode_line, open_or_create_root};

/// How long a batched record may wait before the writer thread commits the batch.
const FLUSH_DELAY: Duration = Duration::from_millis(500);

/// When an appended record must be on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuditDurability {
    /// Written and synced before `append` returns, together with every record before it.
    Immediate,
    /// Committed by the writer thread within `FLUSH_DELAY`, with one sync for the whole batch.
    /// Consecutive records of the same kind that are still pending collapse into one.
    Batched,
}

struct PendingLine {
    id: u64,
    bytes: Vec<u8>,
}

struct AuditState {
    records: VecDeque<AuditRecord>,
    next_sequence: u64,
    pending: Vec<PendingLine>,
    pending_since: Option<Instant>,
    pending_bytes: u64,
    stored_bytes: u64,
    rotated_files: u64,
    write_failures: u64,
    corrupt_records: u64,
    last_error: Option<&'static str>,
    degraded: bool,
    shutdown: bool,
    // Prometheus `_total` families must not fall when retained records are evicted.
    operation_counts: [[u64; 7]; 4],
}

impl AuditState {
    fn new() -> Self {
        Self {
            records: VecDeque::new(),
            next_sequence: 0,
            pending: Vec::new(),
            pending_since: None,
            pending_bytes: 0,
            stored_bytes: 0,
            rotated_files: 0,
            write_failures: 0,
            corrupt_records: 0,
            last_error: None,
            degraded: false,
            shutdown: false,
            operation_counts: [[0; 7]; 4],
        }
    }

    /// The id of the pending tail record that `record` repeats.
    fn mergeable_tail(&self, record: &AuditRecord) -> Option<u64> {
        let tail = self.records.back()?;
        (self.pending.last()?.id == tail.id
            && tail.category == record.category
            && tail.result == record.result
            && tail.operation == record.operation
            && tail.actor == record.actor
            && tail.source == record.source
            && tail.revision == record.revision)
            .then_some(tail.id)
    }

    fn take_pending(&mut self) -> Vec<PendingLine> {
        self.pending_since = None;
        self.pending_bytes = 0;
        std::mem::take(&mut self.pending)
    }

    fn evict(&mut self, limits: &AuditLimits, persistent: bool) {
        while self.records.len() > limits.max_records
            || (!persistent && self.stored_bytes > limits.max_total_bytes)
        {
            let Some(oldest) = self.records.pop_front() else {
                break;
            };
            if !persistent {
                let bytes = encode_line(&oldest, usize::MAX).map_or(0, |line| line.len() as u64);
                self.stored_bytes = self.stored_bytes.saturating_sub(bytes);
            }
        }
    }

    fn record_failure(&mut self, error: AuditStoreError, records: usize, persistent: bool) {
        self.write_failures = self.write_failures.saturating_add(records as u64);
        self.last_error = Some(error.code());
        self.degraded = persistent || self.degraded;
    }

    fn finish_flush(
        &mut self,
        files: &AuditFiles,
        records: usize,
        result: Result<(), AuditStoreError>,
    ) {
        self.stored_bytes = files.total_bytes();
        self.rotated_files = files.rotated_files();
        match result {
            Ok(()) => {
                if let Some(oldest) = files.oldest_id() {
                    while self
                        .records
                        .front()
                        .is_some_and(|record| record.id < oldest)
                    {
                        self.records.pop_front();
                    }
                }
                if self.corrupt_records == 0 {
                    self.last_error = None;
                    self.degraded = false;
                }
            }
            Err(error) => self.record_failure(error, records, true),
        }
    }

    fn status(&self, limits: &AuditLimits, persistent: bool) -> AuditStatus {
        AuditStatus {
            state: if self.degraded {
                AuditComponentState::Degraded
            } else if persistent {
                AuditComponentState::Healthy
            } else {
                AuditComponentState::Memory
            },
            persistent,
            degraded: self.degraded,
            record_count: u64::try_from(self.records.len()).unwrap_or(u64::MAX),
            bytes: self.stored_bytes + self.pending_bytes,
            rotated_files: self.rotated_files,
            max_records: u64::try_from(limits.max_records).unwrap_or(u64::MAX),
            max_record_bytes: u64::try_from(limits.max_record_bytes).unwrap_or(u64::MAX),
            max_file_bytes: limits.max_file_bytes,
            max_total_bytes: limits.max_total_bytes,
            max_rotated_files: u64::try_from(limits.max_rotated_files).unwrap_or(u64::MAX),
            write_failures: self.write_failures,
            corrupt_records: self.corrupt_records,
            last_error: self.last_error,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared {
    limits: AuditLimits,
    state: Mutex<AuditState>,
    wake: Condvar,
    files: Option<Mutex<AuditFiles>>,
}

impl Shared {
    fn new(limits: AuditLimits, state: AuditState, files: Option<AuditFiles>) -> Self {
        Self {
            limits,
            state: Mutex::new(state),
            wake: Condvar::new(),
            files: files.map(Mutex::new),
        }
    }

    /// Writes and syncs every pending record. The file lock is taken first, so batches reach the disk in order.
    fn flush(&self) -> Result<(), AuditStoreError> {
        let Some(files) = &self.files else {
            return Ok(());
        };
        let mut files = lock(files);
        let batch = lock(&self.state).take_pending();
        if batch.is_empty() {
            return Ok(());
        }
        let result = files.write_batch(&batch, &self.limits);
        lock(&self.state).finish_flush(&files, batch.len(), result);
        result
    }
}

/// Bounded audit history: an in-memory ring served to readers, mirrored to size-rotated files by a writer thread.
pub(crate) struct AuditStore {
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
}

impl AuditStore {
    pub(crate) fn memory(limits: AuditLimits) -> Self {
        Self::without_files(limits, AuditState::new())
    }

    pub(crate) fn degraded(limits: AuditLimits, error: &'static str) -> Self {
        let mut state = AuditState::new();
        state.degraded = true;
        state.last_error = Some(error);
        Self::without_files(limits, state)
    }

    fn without_files(limits: AuditLimits, state: AuditState) -> Self {
        Self {
            shared: Arc::new(Shared::new(limits, state, None)),
            writer: None,
        }
    }

    pub(crate) fn open(path: &Path, limits: AuditLimits) -> Result<Self, AuditStoreError> {
        if !limits.valid() {
            return Err(AuditStoreError::InvalidLimits);
        }
        let (mut files, loaded) = AuditFiles::load(open_or_create_root(path)?, &limits)?;
        let mut state = AuditState::new();
        state.records = loaded.records.into();
        state.next_sequence = loaded.max_sequence;
        state.corrupt_records = loaded.corrupt;
        if state.records.len() > limits.max_records {
            let excess = state.records.len() - limits.max_records;
            state.records.drain(..excess);
            files.compact(state.records.make_contiguous(), &limits)?;
        }
        for record in &state.records {
            increment_operation_count(&mut state.operation_counts, record.category, record.result);
        }
        state.stored_bytes = files.total_bytes();
        state.rotated_files = files.rotated_files();
        if loaded.corrupt > 0 {
            state.last_error = Some("corrupt_record_skipped");
            state.degraded = true;
        }
        let shared = Arc::new(Shared::new(limits, state, Some(files)));
        let writer = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("oxiroute-audit".into())
                .spawn(move || writer::run(&shared))
                .map_err(|_| AuditStoreError::Io)?
        };
        Ok(Self {
            shared,
            writer: Some(writer),
        })
    }

    /// Appends a record that is durable when this returns.
    #[cfg(test)]
    pub(crate) fn append(
        &self,
        context: &AuditContext,
        category: AuditCategory,
        operation: &str,
        result: AuditResult,
        revision: Option<&EffectiveRevision>,
    ) -> Result<u64, AuditStoreError> {
        self.append_with(
            AuditDurability::Immediate,
            context,
            category,
            operation,
            result,
            revision,
        )
    }

    pub(crate) fn append_with(
        &self,
        durability: AuditDurability,
        context: &AuditContext,
        category: AuditCategory,
        operation: &str,
        result: AuditResult,
        revision: Option<&EffectiveRevision>,
    ) -> Result<u64, AuditStoreError> {
        let shared = &*self.shared;
        let persistent = shared.files.is_some();
        let mut state = lock(&shared.state);
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
        if durability == AuditDurability::Batched
            && let Some(tail) = state.mergeable_tail(&record)
        {
            increment_operation_count(&mut state.operation_counts, category, result);
            return Ok(tail);
        }
        let bytes = match encode_line(&record, shared.limits.max_record_bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                state.record_failure(error, 1, persistent);
                return Err(error);
            }
        };
        let length = bytes.len() as u64;
        state.next_sequence = id;
        increment_operation_count(&mut state.operation_counts, category, result);
        state.records.push_back(record);
        let mut wake_writer = false;
        if persistent {
            wake_writer = state.pending.is_empty();
            state.pending_since.get_or_insert_with(Instant::now);
            state.pending_bytes += length;
            state.pending.push(PendingLine { id, bytes });
        } else {
            state.stored_bytes += length;
        }
        state.evict(&shared.limits, persistent);
        if state.corrupt_records == 0 && (persistent || !state.degraded) {
            state.last_error = None;
            state.degraded = false;
        }
        drop(state);
        if durability == AuditDurability::Immediate {
            shared.flush()?;
        } else if wake_writer {
            shared.wake.notify_one();
        }
        Ok(id)
    }

    /// Commits every pending record now.
    pub(crate) fn flush(&self) -> Result<(), AuditStoreError> {
        self.shared.flush()
    }

    pub(crate) fn page(
        &self,
        after: u64,
        limit: usize,
        category: Option<AuditCategory>,
        result: Option<AuditResult>,
    ) -> AuditPage {
        let state = lock(&self.shared.state);
        let wanted = |record: &&AuditRecord| {
            category.is_none_or(|category| record.category == category)
                && result.is_none_or(|result| record.result == result)
        };
        let records: Vec<_> = state
            .records
            .iter()
            .filter(|record| record.id > after)
            .filter(wanted)
            .take(limit)
            .cloned()
            .collect();
        let cursor = records.last().map_or(after, |record| record.id);
        let has_more = state
            .records
            .iter()
            .any(|record| record.id > cursor && wanted(&record));
        AuditPage {
            records,
            cursor,
            has_more,
            oldest_cursor: state.records.front().map(|record| record.id),
            latest_cursor: state.next_sequence,
        }
    }

    pub(crate) fn status(&self) -> AuditStatus {
        lock(&self.shared.state).status(&self.shared.limits, self.shared.files.is_some())
    }

    pub(crate) fn metric_snapshot(&self) -> AuditMetricSnapshot {
        let state = lock(&self.shared.state);
        AuditMetricSnapshot {
            status: state.status(&self.shared.limits, self.shared.files.is_some()),
            operation_counts: state.operation_counts,
        }
    }

    #[cfg(test)]
    pub(crate) fn sync_count(&self) -> u64 {
        self.shared
            .files
            .as_ref()
            .map_or(0, |files| lock(files).syncs())
    }
}

impl Drop for AuditStore {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            lock(&self.shared.state).shutdown = true;
            self.shared.wake.notify_all();
            let _ = writer.join();
        }
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

fn increment_operation_count(
    counts: &mut [[u64; 7]; 4],
    category: AuditCategory,
    result: AuditResult,
) {
    let count = &mut counts[category.index()][result.index()];
    *count = count.saturating_add(1);
}
