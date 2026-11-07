#[allow(clippy::wildcard_imports)]
use super::*;

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
    pub(super) fn from_environment() -> Self {
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

    pub(super) fn valid(self) -> bool {
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
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::RootOpen => "root_open_failed",
            Self::UnsafeEntry => "unsafe_entry",
            Self::Io => "io_failed",
            Self::RecordTooLarge => "record_too_large",
        }
    }
}
