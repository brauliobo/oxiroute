use std::{
    collections::VecDeque,
    fs::File,
    io::{Read as _, Write as _},
    os::fd::OwnedFd,
    path::{Component, Path},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::SystemTime,
};

use rustix::{
    fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags, RenameFlags},
    io::Errno,
};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};
use tokio::sync::Notify;

use crate::config_coordinator::EffectiveRevision;
use crate::logging::{next_correlation_id, redact_identifier, redact_text, valid_correlation_id};
use crate::monitoring::{ObservedTransport, TransportOutcome};
use crate::routing::HealthFailure;

pub(crate) const EVENT_CAPACITY: usize = 2_048;
const AUDIT_ACTIVE_FILE: &str = "audit.ndjson";
const AUDIT_TEMP_FILE: &str = "audit.ndjson.tmp";
const AUDIT_ROOT_MODE: u32 = 0o700;
const AUDIT_FILE_MODE: u32 = 0o600;
const DEFAULT_AUDIT_MAX_RECORDS: usize = 10_000;
const DEFAULT_AUDIT_MAX_RECORD_BYTES: usize = 16 * 1024;
const DEFAULT_AUDIT_MAX_FILE_BYTES: u64 = 1024 * 1024;
const DEFAULT_AUDIT_MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_AUDIT_MAX_ROTATED_FILES: usize = 7;
const MAX_AUDIT_RECORDS: usize = 100_000;
const MAX_AUDIT_RECORD_BYTES: usize = 64 * 1024;
const MAX_AUDIT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_AUDIT_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_AUDIT_ROTATED_FILES: usize = 7;
#[allow(clippy::cast_possible_truncation)]
const AUDIT_SCAN_LIMIT_BYTES: usize = MAX_AUDIT_FILE_BYTES as usize + 1;
const CORRELATION_ID_MAX_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuditContext {
    pub correlation_id: String,
    pub actor: String,
    pub source: String,
}

impl AuditContext {
    pub(crate) fn generated() -> Self {
        Self {
            correlation_id: next_correlation_id(),
            actor: "system".into(),
            source: "runtime".into(),
        }
    }

    pub(crate) fn from_external(value: &str) -> Option<Self> {
        if !valid_correlation_id(value) {
            return None;
        }
        Some(Self {
            correlation_id: value.to_owned(),
            actor: "management_bearer".into(),
            source: "management_api".into(),
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditCategory {
    Reload,
    Import,
    Certificate,
    Control,
}

impl AuditCategory {
    pub(crate) const ALL: [Self; 4] =
        [Self::Reload, Self::Import, Self::Certificate, Self::Control];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Reload => 0,
            Self::Import => 1,
            Self::Certificate => 2,
            Self::Control => 3,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Reload => "reload",
            Self::Import => "import",
            Self::Certificate => "certificate",
            Self::Control => "control",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "reload" => Some(Self::Reload),
            "import" => Some(Self::Import),
            "certificate" => Some(Self::Certificate),
            "control" => Some(Self::Control),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditResult {
    Requested,
    Succeeded,
    Failed,
    Rejected,
    Conflict,
    Partial,
    Degraded,
}

impl AuditResult {
    pub(crate) const ALL: [Self; 7] = [
        Self::Requested,
        Self::Succeeded,
        Self::Failed,
        Self::Rejected,
        Self::Conflict,
        Self::Partial,
        Self::Degraded,
    ];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Requested => 0,
            Self::Succeeded => 1,
            Self::Failed => 2,
            Self::Rejected => 3,
            Self::Conflict => 4,
            Self::Partial => 5,
            Self::Degraded => 6,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Conflict => "conflict",
            Self::Partial => "partial",
            Self::Degraded => "degraded",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "rejected" => Some(Self::Rejected),
            "conflict" => Some(Self::Conflict),
            "partial" => Some(Self::Partial),
            "degraded" => Some(Self::Degraded),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditRecord {
    pub id: u64,
    pub timestamp_unix_ms: u64,
    pub correlation_id: String,
    pub actor: String,
    pub source: String,
    pub category: AuditCategory,
    pub operation: String,
    pub result: AuditResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditComponentState {
    Healthy,
    Degraded,
    Memory,
}

impl AuditComponentState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Memory => "memory",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditStatus {
    pub state: AuditComponentState,
    pub persistent: bool,
    pub degraded: bool,
    pub record_count: u64,
    pub bytes: u64,
    pub rotated_files: u64,
    pub max_records: u64,
    pub max_record_bytes: u64,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_rotated_files: u64,
    pub write_failures: u64,
    pub corrupt_records: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub(crate) struct AuditPage {
    pub records: Vec<AuditRecord>,
    pub cursor: u64,
    pub has_more: bool,
    pub oldest_cursor: Option<u64>,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct AuditMetricSnapshot {
    pub status: AuditStatus,
    pub operation_counts: [[u64; 7]; 4],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_field_names)]
pub(crate) struct AuditLimits {
    pub max_records: usize,
    pub max_record_bytes: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_rotated_files: usize,
}

impl Default for AuditLimits {
    fn default() -> Self {
        Self {
            max_records: DEFAULT_AUDIT_MAX_RECORDS,
            max_record_bytes: DEFAULT_AUDIT_MAX_RECORD_BYTES,
            max_file_bytes: DEFAULT_AUDIT_MAX_FILE_BYTES,
            max_total_bytes: DEFAULT_AUDIT_MAX_TOTAL_BYTES,
            max_rotated_files: DEFAULT_AUDIT_MAX_ROTATED_FILES,
        }
    }
}

impl AuditLimits {
    fn from_environment() -> Self {
        let defaults = Self::default();
        let limits = Self {
            max_records: environment_usize("OXIROUTE_AUDIT_MAX_RECORDS", defaults.max_records)
                .min(MAX_AUDIT_RECORDS),
            max_record_bytes: environment_usize(
                "OXIROUTE_AUDIT_MAX_RECORD_BYTES",
                defaults.max_record_bytes,
            )
            .min(MAX_AUDIT_RECORD_BYTES),
            max_file_bytes: environment_u64(
                "OXIROUTE_AUDIT_MAX_FILE_BYTES",
                defaults.max_file_bytes,
            )
            .min(MAX_AUDIT_FILE_BYTES),
            max_total_bytes: environment_u64(
                "OXIROUTE_AUDIT_MAX_TOTAL_BYTES",
                defaults.max_total_bytes,
            )
            .min(MAX_AUDIT_TOTAL_BYTES),
            max_rotated_files: environment_usize(
                "OXIROUTE_AUDIT_MAX_ROTATED_FILES",
                defaults.max_rotated_files,
            )
            .min(MAX_AUDIT_ROTATED_FILES),
        };
        if limits.valid() { limits } else { defaults }
    }

    fn valid(self) -> bool {
        self.max_records > 0
            && self.max_record_bytes > 0
            && self.max_file_bytes > 0
            && self.max_total_bytes >= self.max_file_bytes
            && self.max_rotated_files > 0
            && u64::try_from(self.max_record_bytes).is_ok_and(|bytes| bytes <= self.max_file_bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuditStoreError {
    InvalidLimits,
    RootOpen,
    UnsafeEntry,
    Io,
    RecordTooLarge,
}

impl AuditStoreError {
    const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::RootOpen => "root_open_failed",
            Self::UnsafeEntry => "unsafe_entry",
            Self::Io => "io_failed",
            Self::RecordTooLarge => "record_too_large",
        }
    }
}

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
    fn new() -> Self {
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

fn environment_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn environment_u64(name: &str, default: u64) -> u64 {
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationalEvent {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: EventName,
    pub outcome: EventOutcome,
    pub revision: Option<EffectiveRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum EventName {
    GenerationPrepare,
    GenerationActivate,
    GenerationRollback,
    GenerationDrain,
    GenerationStart,
    ConfigurationReload,
    ImportCompleted,
    ControlOperation,
    ProcessShutdown,
    ListenerAdministrativeState,
    PoolAdministrativeState,
    ServerUpdate,
    RtmpConnect,
    RtmpPublish,
    RtmpPlay,
    RtmpDisconnect,
    RtmpAccess,
    CertificateRenewal,
    CertificateActivation,
    CertificateRevocation,
    CertificateDeletion,
    CertificateAccountRollover,
    CertificateJobControl,
    UpstreamEndpointEjection,
    UpstreamEndpointRecovery,
    Unknown,
}

impl Serialize for EventName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(v1_serialized_event_name(*self))
    }
}

impl EventName {
    fn parse(value: &str) -> Self {
        match value {
            "generation_prepare" => Self::GenerationPrepare,
            "generation_activate" => Self::GenerationActivate,
            "generation_rollback" => Self::GenerationRollback,
            "generation_drain" => Self::GenerationDrain,
            "generation_start" => Self::GenerationStart,
            "configuration_reload" => Self::ConfigurationReload,
            "import_completed" => Self::ImportCompleted,
            "control_operation" => Self::ControlOperation,
            "process_shutdown" => Self::ProcessShutdown,
            "listener_administrative_state" => Self::ListenerAdministrativeState,
            "pool_administrative_state" => Self::PoolAdministrativeState,
            "server_update" => Self::ServerUpdate,
            "rtmp_connect" => Self::RtmpConnect,
            "rtmp_publish" => Self::RtmpPublish,
            "rtmp_play" => Self::RtmpPlay,
            "rtmp_disconnect" => Self::RtmpDisconnect,
            "rtmp_access" => Self::RtmpAccess,
            "certificate_renewal" => Self::CertificateRenewal,
            "certificate_activated" => Self::CertificateActivation,
            "certificate_revocation" => Self::CertificateRevocation,
            "certificate_deletion" => Self::CertificateDeletion,
            "certificate_account_rollover" => Self::CertificateAccountRollover,
            "certificate_job_control" => Self::CertificateJobControl,
            "upstream_endpoint_ejection" => Self::UpstreamEndpointEjection,
            "upstream_endpoint_recovery" => Self::UpstreamEndpointRecovery,
            _ => Self::Unknown,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GenerationPrepare => "generation_prepare",
            Self::GenerationActivate => "generation_activate",
            Self::GenerationRollback => "generation_rollback",
            Self::GenerationDrain => "generation_drain",
            Self::GenerationStart => "generation_start",
            Self::ConfigurationReload => "configuration_reload",
            Self::ImportCompleted => "import_completed",
            Self::ControlOperation => "control_operation",
            Self::ProcessShutdown => "process_shutdown",
            Self::ListenerAdministrativeState => "listener_administrative_state",
            Self::PoolAdministrativeState => "pool_administrative_state",
            Self::ServerUpdate => "server_update",
            Self::RtmpConnect => "rtmp_connect",
            Self::RtmpPublish => "rtmp_publish",
            Self::RtmpPlay => "rtmp_play",
            Self::RtmpDisconnect => "rtmp_disconnect",
            Self::RtmpAccess => "rtmp_access",
            Self::CertificateRenewal => "certificate_renewal",
            Self::CertificateActivation => "certificate_activated",
            Self::CertificateRevocation => "certificate_revocation",
            Self::CertificateDeletion => "certificate_deletion",
            Self::CertificateAccountRollover => "certificate_account_rollover",
            Self::CertificateJobControl => "certificate_job_control",
            Self::UpstreamEndpointEjection => "upstream_endpoint_ejection",
            Self::UpstreamEndpointRecovery => "upstream_endpoint_recovery",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) const fn v1_serialized_event_name(event: EventName) -> &'static str {
    match event {
        EventName::CertificateActivation => "certificate_activation",
        EventName::CertificateRevocation
        | EventName::CertificateDeletion
        | EventName::CertificateAccountRollover
        | EventName::CertificateJobControl => "unknown",
        _ => event.as_str(),
    }
}

pub(crate) const fn v1_sse_event_name(event: EventName) -> &'static str {
    match event {
        EventName::CertificateRevocation
        | EventName::CertificateDeletion
        | EventName::CertificateAccountRollover
        | EventName::CertificateJobControl => "unknown",
        _ => event.as_str(),
    }
}

pub(crate) fn v1_event_value(event: &OperationalEvent) -> serde_json::Value {
    let mut value = serde_json::to_value(event).expect("typed operational event serializes");
    value["event"] = v1_serialized_event_name(event.event).into();
    value
}

pub(crate) fn v2_event_value(event: &OperationalEvent) -> serde_json::Value {
    let mut value = serde_json::to_value(event).expect("typed operational event serializes");
    value["event"] = event.event.as_str().into();
    value
}

#[derive(Clone, Debug)]
pub(crate) enum EventOutcome {
    Prepared,
    Rejected,
    Activated,
    Quarantined,
    Requested,
    Applied,
    Failed,
    Ejected {
        pool: String,
        server: String,
        reason: HealthFailure,
        failure_count: u64,
        ejection_count: u64,
        ejected_at_unix_ms: u64,
        ejection_until_unix_ms: u64,
    },
    Recovered {
        pool: String,
        server: String,
        reason: Option<HealthFailure>,
        recovery_count: u64,
        recovered_at_unix_ms: u64,
    },
    Unknown,
}

impl Serialize for EventOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Prepared => serializer.serialize_str("prepared"),
            Self::Rejected => serializer.serialize_str("rejected"),
            Self::Activated => serializer.serialize_str("activated"),
            Self::Quarantined => serializer.serialize_str("quarantined"),
            Self::Requested => serializer.serialize_str("requested"),
            Self::Applied => serializer.serialize_str("applied"),
            Self::Failed => serializer.serialize_str("failed"),
            Self::Unknown => serializer.serialize_str("unknown"),
            Self::Ejected {
                pool,
                server,
                reason,
                failure_count,
                ejection_count,
                ejected_at_unix_ms,
                ejection_until_unix_ms,
            } => {
                let mut value = serializer.serialize_struct("EventOutcome", 8)?;
                value.serialize_field("type", "ejected")?;
                value.serialize_field("pool", pool)?;
                value.serialize_field("server", server)?;
                value.serialize_field("reason", reason)?;
                value.serialize_field("failureCount", failure_count)?;
                value.serialize_field("ejectionCount", ejection_count)?;
                value.serialize_field("ejectedAtUnixMs", ejected_at_unix_ms)?;
                value.serialize_field("ejectionUntilUnixMs", ejection_until_unix_ms)?;
                value.end()
            }
            Self::Recovered {
                pool,
                server,
                reason,
                recovery_count,
                recovered_at_unix_ms,
            } => {
                let mut value = serializer.serialize_struct("EventOutcome", 6)?;
                value.serialize_field("type", "recovered")?;
                value.serialize_field("pool", pool)?;
                value.serialize_field("server", server)?;
                value.serialize_field("reason", reason)?;
                value.serialize_field("recoveryCount", recovery_count)?;
                value.serialize_field("recoveredAtUnixMs", recovered_at_unix_ms)?;
                value.end()
            }
        }
    }
}

impl EventOutcome {
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Rejected => "rejected",
            Self::Activated => "activated",
            Self::Quarantined => "quarantined",
            Self::Requested => "requested",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Ejected { .. } => "ejected",
            Self::Recovered { .. } => "recovered",
            Self::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "prepared" => Self::Prepared,
            "rejected" => Self::Rejected,
            "activated" => Self::Activated,
            "quarantined" => Self::Quarantined,
            "requested" => Self::Requested,
            "applied" => Self::Applied,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

pub(crate) struct EventPage {
    pub events: Vec<OperationalEvent>,
    pub cursor: u64,
    pub has_more: bool,
    pub oldest_cursor: Option<u64>,
    pub latest_cursor: u64,
    pub cursor_lost: bool,
}

/// Redacted event data safe to transfer to a supervised worker.
#[derive(Clone, Debug)]
pub struct WorkerEventSnapshot {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: String,
    pub outcome: String,
    pub revision: Option<String>,
    pub certificate: Option<String>,
    pub correlation_id: Option<String>,
    pub source: Option<String>,
    pub operation: Option<String>,
}

/// Bounded event page exposed to the supervised worker adapter.
#[derive(Clone, Debug)]
pub struct WorkerEventPage {
    pub events: Vec<WorkerEventSnapshot>,
    pub cursor: u64,
    pub has_more: bool,
    pub oldest_cursor: Option<u64>,
    pub latest_cursor: u64,
    pub cursor_lost: bool,
}

#[derive(Default)]
struct EventLog {
    next_cursor: u64,
    events: VecDeque<OperationalEvent>,
}

fn log() -> &'static Mutex<EventLog> {
    static LOG: OnceLock<Mutex<EventLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(EventLog::default()))
}

fn notifications() -> &'static Notify {
    static NOTIFICATIONS: OnceLock<Notify> = OnceLock::new();
    NOTIFICATIONS.get_or_init(Notify::new)
}

fn audit_registry() -> &'static RwLock<Option<Arc<AuditStore>>> {
    static REGISTRY: OnceLock<RwLock<Option<Arc<AuditStore>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(None))
}

pub(crate) fn configure_audit_store(token_file: Option<&Path>) -> Arc<AuditStore> {
    let limits = AuditLimits::from_environment();
    let path = std::env::var_os("OXIROUTE_AUDIT_DIR")
        .map(Into::into)
        .or_else(|| {
            token_file
                .and_then(Path::parent)
                .map(|parent| parent.join("audit"))
        });
    let store = path.map_or_else(
        || AuditStore::memory(limits),
        |path| match AuditStore::open(&path, limits) {
            Ok(store) => store,
            Err(error) => AuditStore::degraded(limits, error.code()),
        },
    );
    let store = Arc::new(store);
    *audit_registry()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&store));
    store
}

fn current_audit_store() -> Arc<AuditStore> {
    if let Some(store) = audit_registry()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .cloned()
    {
        return store;
    }
    configure_audit_store(None)
}

pub(crate) fn audit_status() -> AuditStatus {
    current_audit_store().status()
}

pub(crate) fn audit_metrics() -> AuditMetricSnapshot {
    current_audit_store().metric_snapshot()
}

pub(crate) fn emit(event: &str, outcome: &str, revision: Option<&EffectiveRevision>) {
    emit_with_context(event, outcome, revision, &AuditContext::generated());
}

/// Emits a bounded, redacted RTMP lifecycle event without copying stream queries or credentials.
pub fn emit_rtmp_access(event: &str, outcome: &str) {
    crate::monitoring::record_transport_event(
        ObservedTransport::Rtmp,
        match outcome {
            "accepted" | "closed" => TransportOutcome::Success,
            "rejected" => TransportOutcome::Rejected,
            _ => TransportOutcome::InternalError,
        },
    );
    let event = match event {
        "connect" => "rtmp_connect",
        "publish" => "rtmp_publish",
        "play" => "rtmp_play",
        "disconnect" => "rtmp_disconnect",
        _ => "rtmp_access",
    };
    let outcome = match outcome {
        "accepted" | "closed" => "applied",
        "rejected" => "rejected",
        _ => "failed",
    };
    emit_inner(
        event,
        outcome,
        None,
        None,
        &AuditContext::generated(),
        None,
        None,
    );
}

pub fn emit_certificate(event: &str, outcome: &str, certificate: &str) {
    crate::monitoring::record_transport_event(
        ObservedTransport::Acme,
        match outcome {
            "requested" | "activated" | "applied" => TransportOutcome::Success,
            "failed" => TransportOutcome::UpstreamError,
            "rejected" => TransportOutcome::Rejected,
            _ => TransportOutcome::InternalError,
        },
    );
    emit_certificate_with_context(event, outcome, certificate, &AuditContext::generated());
}

pub(crate) fn emit_with_context(
    event: &str,
    outcome: &str,
    revision: Option<&EffectiveRevision>,
    context: &AuditContext,
) {
    emit_inner(event, outcome, revision, None, context, None, None);
}

pub(crate) fn emit_certificate_with_context(
    event: &str,
    outcome: &str,
    certificate: &str,
    context: &AuditContext,
) {
    emit_inner(event, outcome, None, Some(certificate), context, None, None);
}

pub(crate) fn emit_api_operation(
    operation: &str,
    category: AuditCategory,
    result: AuditResult,
    revision: Option<&EffectiveRevision>,
    context: &AuditContext,
) {
    let event = match operation {
        "generation_reload" => "generation_prepare",
        "generation_rollback" => "generation_rollback",
        "generation_drain" => "generation_drain",
        "configuration_reload" => "configuration_reload",
        "certificate_reconcile" | "certificate_renew" => "certificate_renewal",
        "certificate_revoke" => "certificate_revocation",
        "certificate_delete" => "certificate_deletion",
        "account_key_rollover" => "certificate_account_rollover",
        "certificate_job_control" => "certificate_job_control",
        "process_shutdown" => "process_shutdown",
        "listener_control" => "listener_administrative_state",
        "pool_control" => "pool_administrative_state",
        "server_control" | "server_dns_refresh" => "server_update",
        _ => "control_operation",
    };
    let outcome = match result {
        AuditResult::Requested => "requested",
        AuditResult::Succeeded | AuditResult::Partial => "applied",
        AuditResult::Failed | AuditResult::Degraded => "failed",
        AuditResult::Rejected | AuditResult::Conflict => "rejected",
    };
    emit_inner(
        event,
        outcome,
        revision,
        None,
        context,
        Some(operation),
        Some((category, result)),
    );
}

pub(crate) fn emit_upstream_endpoint_ejection(
    pool: &str,
    server: &str,
    reason: HealthFailure,
    failure_count: u64,
    ejection_count: u64,
    ejected_at_unix_ms: u64,
    ejection_until_unix_ms: u64,
) {
    emit_typed(
        EventName::UpstreamEndpointEjection,
        EventOutcome::Ejected {
            pool: redact_identifier(pool),
            server: redact_identifier(server),
            reason,
            failure_count,
            ejection_count,
            ejected_at_unix_ms,
            ejection_until_unix_ms,
        },
    );
}

pub(crate) fn emit_upstream_endpoint_recovery(
    pool: &str,
    server: &str,
    reason: Option<HealthFailure>,
    recovery_count: u64,
    recovered_at_unix_ms: u64,
) {
    emit_typed(
        EventName::UpstreamEndpointRecovery,
        EventOutcome::Recovered {
            pool: redact_identifier(pool),
            server: redact_identifier(server),
            reason,
            recovery_count,
            recovered_at_unix_ms,
        },
    );
}

fn emit_typed(event: EventName, outcome: EventOutcome) {
    let context = AuditContext::generated();
    let category = category_for_event(event);
    let result = result_for_outcome(&outcome);
    let mut state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut value = OperationalEvent {
        cursor: 0,
        timestamp_unix_ms: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        event,
        outcome,
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let audit_value = publish(&mut state, &mut value, &context, None);
    drop(state);
    persist(audit_value, &context, category, result);
}

fn emit_inner(
    event: &str,
    outcome: &str,
    revision: Option<&EffectiveRevision>,
    certificate: Option<&str>,
    context: &AuditContext,
    operation: Option<&str>,
    category_result: Option<(AuditCategory, AuditResult)>,
) {
    let mut state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut value = OperationalEvent::new(
        0,
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        event,
        outcome,
        revision,
    );
    value.certificate = certificate.map(redact_identifier);
    let event_category =
        category_result.map_or_else(|| category_for_event(value.event), |(category, _)| category);
    let event_result =
        category_result.map_or_else(|| result_for_outcome(&value.outcome), |(_, result)| result);
    let audit_value = publish(&mut state, &mut value, context, operation);
    drop(state);
    persist(audit_value, context, event_category, event_result);
}

fn publish(
    state: &mut EventLog,
    value: &mut OperationalEvent,
    context: &AuditContext,
    operation: Option<&str>,
) -> OperationalEvent {
    state.next_cursor = state.next_cursor.saturating_add(1);
    value.cursor = state.next_cursor;
    value.correlation_id = Some(redact_identifier(&context.correlation_id));
    value.actor = Some(redact_identifier(&context.actor));
    value.source = Some(redact_identifier(&context.source));
    value.operation = Some(redact_identifier(operation.unwrap_or(value.event.as_str())));
    if state.events.len() == EVENT_CAPACITY {
        state.events.pop_front();
    }
    state.events.push_back(value.clone());
    value.clone()
}

#[allow(clippy::needless_pass_by_value)]
fn persist(
    audit_value: OperationalEvent,
    context: &AuditContext,
    category: AuditCategory,
    result: AuditResult,
) {
    let revision = audit_value.revision.as_ref();
    if let Err(error) = current_audit_store().append(
        context,
        category,
        audit_value.operation.as_deref().unwrap_or("unknown"),
        result,
        revision,
    ) {
        log::warn!(target: "oxiroute::audit", "durable audit write failed: {}", error.code());
    }
    if let Ok(json) = serde_json::to_string(&audit_value) {
        log::info!(target: "oxiroute::operations", "{json}");
    }
    notifications().notify_one();
}

fn unix_time_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

const fn category_for_event(event: EventName) -> AuditCategory {
    match event {
        EventName::GenerationPrepare
        | EventName::GenerationActivate
        | EventName::GenerationRollback
        | EventName::GenerationDrain
        | EventName::GenerationStart
        | EventName::ConfigurationReload => AuditCategory::Reload,
        EventName::ImportCompleted => AuditCategory::Import,
        EventName::CertificateRenewal
        | EventName::CertificateActivation
        | EventName::CertificateRevocation
        | EventName::CertificateDeletion
        | EventName::CertificateAccountRollover
        | EventName::CertificateJobControl => AuditCategory::Certificate,
        EventName::ControlOperation
        | EventName::ProcessShutdown
        | EventName::ListenerAdministrativeState
        | EventName::PoolAdministrativeState
        | EventName::ServerUpdate
        | EventName::RtmpConnect
        | EventName::RtmpPublish
        | EventName::RtmpPlay
        | EventName::RtmpDisconnect
        | EventName::RtmpAccess
        | EventName::UpstreamEndpointEjection
        | EventName::UpstreamEndpointRecovery
        | EventName::Unknown => AuditCategory::Control,
    }
}

const fn result_for_outcome(outcome: &EventOutcome) -> AuditResult {
    match outcome {
        EventOutcome::Prepared | EventOutcome::Activated | EventOutcome::Applied => {
            AuditResult::Succeeded
        }
        EventOutcome::Requested => AuditResult::Requested,
        EventOutcome::Rejected => AuditResult::Rejected,
        EventOutcome::Failed | EventOutcome::Quarantined | EventOutcome::Unknown => {
            AuditResult::Failed
        }
        EventOutcome::Ejected { .. } | EventOutcome::Recovered { .. } => AuditResult::Succeeded,
    }
}

pub(crate) fn page(after: u64, limit: usize) -> EventPage {
    let state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.page(after, limit)
}

/// Returns a bounded, redacted event page without exposing internal event enums.
#[must_use]
pub fn worker_event_page(after: u64, limit: usize) -> WorkerEventPage {
    let EventPage {
        events,
        cursor,
        has_more,
        oldest_cursor,
        latest_cursor,
        cursor_lost,
    } = page(after, limit);
    WorkerEventPage {
        events: events
            .into_iter()
            .map(|event| WorkerEventSnapshot {
                cursor: event.cursor,
                timestamp_unix_ms: event.timestamp_unix_ms,
                event: event.event.as_str().to_owned(),
                outcome: event.outcome.as_str().to_owned(),
                revision: event.revision.map(|revision| revision.to_string()),
                certificate: event.certificate,
                correlation_id: event.correlation_id,
                source: event.source,
                operation: event.operation,
            })
            .collect(),
        cursor,
        has_more,
        oldest_cursor,
        latest_cursor,
        cursor_lost,
    }
}

pub(crate) fn current_cursor() -> u64 {
    log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .next_cursor
}

pub(crate) async fn wait_for_event() {
    notifications().notified().await;
}

impl EventLog {
    fn page(&self, after: u64, limit: usize) -> EventPage {
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.cursor > after)
            .take(limit.min(EVENT_CAPACITY))
            .cloned()
            .collect();
        let cursor = events.last().map_or(after, |event| event.cursor);
        let has_more = self.events.iter().any(|event| event.cursor > cursor);
        let oldest_cursor = self.events.front().map(|event| event.cursor);
        EventPage {
            events,
            cursor,
            has_more,
            oldest_cursor,
            latest_cursor: self.next_cursor,
            cursor_lost: oldest_cursor.is_some_and(|oldest| after < oldest.saturating_sub(1)),
        }
    }
}

impl OperationalEvent {
    fn new(
        cursor: u64,
        timestamp_unix_ms: Option<u64>,
        event: &str,
        outcome: &str,
        revision: Option<&EffectiveRevision>,
    ) -> Self {
        Self {
            cursor,
            timestamp_unix_ms,
            event: EventName::parse(event),
            outcome: EventOutcome::parse(outcome),
            revision: revision.cloned(),
            certificate: None,
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write as _,
        os::unix::fs::PermissionsExt,
    };

    use tempfile::tempdir;

    use super::*;

    fn audit_limits() -> AuditLimits {
        AuditLimits {
            max_records: 2,
            max_record_bytes: 4 * 1024,
            max_file_bytes: 8 * 1024,
            max_total_bytes: 32 * 1024,
            max_rotated_files: 2,
        }
    }

    fn audit_directory() -> tempfile::TempDir {
        let directory = tempdir().expect("audit directory");
        fs::set_permissions(
            directory.path(),
            fs::Permissions::from_mode(AUDIT_ROOT_MODE),
        )
        .expect("secure audit directory");
        directory
    }

    #[test]
    fn external_correlation_ids_are_bounded_and_safe() {
        assert!(AuditContext::from_external("request-42").is_some());
        assert!(AuditContext::from_external(&"x".repeat(65)).is_none());
        assert!(AuditContext::from_external("request id").is_none());
        assert!(AuditContext::from_external("Authorization: Bearer secret").is_none());
    }

    #[test]
    fn durable_audit_records_round_trip_with_filters_and_retention() {
        let directory = audit_directory();
        let limits = audit_limits();
        let context = AuditContext::from_external("request-42").expect("safe context");
        let store = AuditStore::open(directory.path(), limits).expect("open audit store");
        store
            .append(
                &context,
                AuditCategory::Reload,
                "generation_reload",
                AuditResult::Succeeded,
                None,
            )
            .expect("reload record");
        store
            .append(
                &context,
                AuditCategory::Control,
                "listener_control",
                AuditResult::Rejected,
                None,
            )
            .expect("control record");
        store
            .append(
                &context,
                AuditCategory::Certificate,
                "certificate_renew",
                AuditResult::Requested,
                None,
            )
            .expect("certificate record");

        let page = store.page(0, 10, Some(AuditCategory::Certificate), None);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].operation, "certificate_renew");
        assert_eq!(store.status().record_count, 2);
        drop(store);

        let reopened = AuditStore::open(directory.path(), limits).expect("reopen audit store");
        let page = reopened.page(0, 10, None, None);
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records[0].id, 2);
        assert_eq!(page.records[1].id, 3);
        assert_eq!(page.records[0].actor, "management_bearer");
        assert_eq!(page.records[0].source, "management_api");
    }

    #[test]
    fn audit_operation_totals_remain_monotonic_after_retention() {
        let store = AuditStore::memory(audit_limits());
        let context = AuditContext::generated();

        for _ in 0..2 {
            store
                .append(
                    &context,
                    AuditCategory::Reload,
                    "generation_reload",
                    AuditResult::Requested,
                    None,
                )
                .expect("reload record");
        }
        store
            .append(
                &context,
                AuditCategory::Control,
                "process_shutdown",
                AuditResult::Rejected,
                None,
            )
            .expect("control record");

        let metrics = store.metric_snapshot();
        assert_eq!(metrics.status.record_count, 2);
        assert_eq!(
            metrics.operation_counts[AuditCategory::Reload.index()][AuditResult::Requested.index()],
            2
        );
        assert_eq!(
            metrics.operation_counts[AuditCategory::Control.index()][AuditResult::Rejected.index()],
            1
        );
    }

    #[test]
    fn corrupt_audit_lines_are_skipped_and_reported_as_degraded() {
        let directory = audit_directory();
        let limits = audit_limits();
        let context = AuditContext::generated();
        let store = AuditStore::open(directory.path(), limits).expect("open audit store");
        store
            .append(
                &context,
                AuditCategory::Control,
                "process_drain",
                AuditResult::Requested,
                None,
            )
            .expect("audit record");
        drop(store);

        let mut file = OpenOptions::new()
            .append(true)
            .open(directory.path().join(AUDIT_ACTIVE_FILE))
            .expect("active audit file");
        file.write_all(b"not-json\n").expect("corrupt line");
        drop(file);

        let reopened = AuditStore::open(directory.path(), limits).expect("recover audit store");
        let status = reopened.status();
        assert!(status.degraded);
        assert_eq!(status.corrupt_records, 1);
        assert_eq!(reopened.page(0, 10, None, None).records.len(), 1);
    }

    #[test]
    fn bounded_pages_advance_to_the_last_returned_event() {
        let mut log = EventLog::default();
        for cursor in 1..=5 {
            log.events
                .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
            log.next_cursor = cursor;
        }

        let first = log.page(0, 2);
        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(first.cursor, 2);
        assert!(first.has_more);
        assert_eq!(first.oldest_cursor, Some(1));
        assert!(!first.cursor_lost);

        let second = log.page(first.cursor, 2);
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(second.cursor, 4);
        assert!(second.has_more);
        assert_eq!(second.latest_cursor, 5);
    }

    #[test]
    fn an_initial_page_uses_the_current_cursor_without_replay() {
        let mut log = EventLog::default();
        for cursor in 1..=5 {
            log.events
                .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
            log.next_cursor = cursor;
        }

        let page = log.page(log.next_cursor, 64);
        assert!(page.events.is_empty());
        assert_eq!(page.cursor, 5);
        assert_eq!(page.latest_cursor, 5);
        assert!(!page.cursor_lost);
    }

    #[test]
    fn reports_cursor_loss_only_after_the_last_evicted_cursor() {
        let mut log = EventLog::default();
        for cursor in 1..=(EVENT_CAPACITY as u64 + 2) {
            log.events
                .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
            if log.events.len() > EVENT_CAPACITY {
                log.events.pop_front();
            }
            log.next_cursor = cursor;
        }

        assert_eq!(log.events.front().map(|event| event.cursor), Some(3));
        assert!(log.page(1, 10).cursor_lost);
        assert!(!log.page(2, 10).cursor_lost);
        assert_eq!(log.page(2, 10).events[0].cursor, 3);
    }

    #[test]
    fn unknown_event_values_are_serialized_as_safe_typed_values() {
        let event = OperationalEvent::new(
            1,
            None,
            "Authorization: Bearer private-key-secret",
            "Cookie=session-secret",
            None,
        );
        let json = serde_json::to_string(&event).expect("event JSON");

        assert!(json.contains(r#""event":"unknown""#));
        assert!(json.contains(r#""outcome":"unknown""#));
        assert!(!json.contains("private-key-secret"));
        assert!(!json.contains("session-secret"));
    }

    #[test]
    fn certificate_operation_event_names_round_trip_in_the_certificate_category() {
        for name in [
            "certificate_revocation",
            "certificate_deletion",
            "certificate_account_rollover",
            "certificate_job_control",
        ] {
            let event = OperationalEvent::new(1, None, name, "requested", None);

            assert_eq!(event.event.as_str(), name);
            assert_eq!(category_for_event(event.event), AuditCategory::Certificate);
        }
    }

    #[test]
    fn default_serialization_preserves_persisted_version_one_event_names() {
        let mut event = OperationalEvent {
            cursor: 1,
            timestamp_unix_ms: None,
            event: EventName::CertificateActivation,
            outcome: EventOutcome::Activated,
            revision: None,
            certificate: Some("edge".into()),
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };

        assert_eq!(
            serde_json::to_string(&event).expect("event JSON"),
            r#"{"cursor":1,"timestampUnixMs":null,"event":"certificate_activation","outcome":"activated","revision":null,"certificate":"edge"}"#
        );

        event.event = EventName::CertificateRevocation;
        event.outcome = EventOutcome::Requested;
        assert_eq!(
            serde_json::to_string(&event).expect("event JSON"),
            r#"{"cursor":1,"timestampUnixMs":null,"event":"unknown","outcome":"requested","revision":null,"certificate":"edge"}"#
        );
    }

    #[test]
    fn version_one_projection_preserves_shipped_event_names() {
        let mut event = OperationalEvent {
            cursor: 1,
            timestamp_unix_ms: None,
            event: EventName::CertificateActivation,
            outcome: EventOutcome::Activated,
            revision: None,
            certificate: Some("edge".into()),
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };

        assert_eq!(v1_event_value(&event)["event"], "certificate_activation");
        event.event = EventName::CertificateRevocation;
        event.outcome = EventOutcome::Requested;
        assert_eq!(v1_event_value(&event)["event"], "unknown");
        assert_eq!(v1_event_value(&event)["outcome"], "requested");
    }

    #[test]
    fn version_two_projection_uses_the_authoritative_event_names() {
        let mut event = OperationalEvent {
            cursor: 1,
            timestamp_unix_ms: None,
            event: EventName::CertificateActivation,
            outcome: EventOutcome::Activated,
            revision: None,
            certificate: Some("edge".into()),
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };

        assert_eq!(v2_event_value(&event)["event"], "certificate_activated");
        event.event = EventName::CertificateRevocation;
        assert_eq!(v2_event_value(&event)["event"], "certificate_revocation");
    }

    #[test]
    fn passive_endpoint_events_serialize_bounded_recovery_details() {
        let ejection = OperationalEvent {
            cursor: 1,
            timestamp_unix_ms: Some(10),
            event: EventName::UpstreamEndpointEjection,
            outcome: EventOutcome::Ejected {
                pool: "backend".into(),
                server: "primary".into(),
                reason: HealthFailure::ConnectFailed,
                failure_count: 3,
                ejection_count: 2,
                ejected_at_unix_ms: 10,
                ejection_until_unix_ms: 20,
            },
            revision: None,
            certificate: None,
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };
        let recovery = OperationalEvent {
            cursor: 2,
            timestamp_unix_ms: Some(30),
            event: EventName::UpstreamEndpointRecovery,
            outcome: EventOutcome::Recovered {
                pool: "backend".into(),
                server: "primary".into(),
                reason: Some(HealthFailure::ConnectFailed),
                recovery_count: 1,
                recovered_at_unix_ms: 30,
            },
            revision: None,
            certificate: None,
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };

        let ejection = serde_json::to_value(ejection).expect("ejection event JSON");
        let recovery = serde_json::to_value(recovery).expect("recovery event JSON");
        assert_eq!(ejection["outcome"]["type"], "ejected");
        assert_eq!(ejection["outcome"]["reason"], "connect_failed");
        assert_eq!(ejection["outcome"]["ejectionCount"], 2);
        assert_eq!(recovery["outcome"]["type"], "recovered");
        assert_eq!(recovery["outcome"]["recoveryCount"], 1);
    }
}
