use schemars::JsonSchema;
use serde::Serialize;

use crate::operational_event::{
    AuditCategory, AuditComponentState, AuditPage, AuditRecord, AuditResult, AuditStatus,
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditPageResponse {
    records: Vec<AuditRecordDto>,
    cursor: u64,
    has_more: bool,
    oldest_cursor: Option<u64>,
    latest_cursor: u64,
}

impl From<AuditPage> for AuditPageResponse {
    fn from(page: AuditPage) -> Self {
        Self {
            records: page.records.into_iter().map(Into::into).collect(),
            cursor: page.cursor,
            has_more: page.has_more,
            oldest_cursor: page.oldest_cursor,
            latest_cursor: page.latest_cursor,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditRecordDto {
    id: u64,
    timestamp_unix_ms: u64,
    correlation_id: String,
    actor: String,
    source: String,
    category: AuditCategoryDto,
    operation: String,
    result: AuditResultDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

impl From<AuditRecord> for AuditRecordDto {
    fn from(record: AuditRecord) -> Self {
        Self {
            id: record.id,
            timestamp_unix_ms: record.timestamp_unix_ms,
            correlation_id: record.correlation_id,
            actor: record.actor,
            source: record.source,
            category: record.category.into(),
            operation: record.operation,
            result: record.result.into(),
            revision: record.revision,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuditCategoryDto {
    Reload,
    Import,
    Certificate,
    Control,
}

impl From<AuditCategory> for AuditCategoryDto {
    fn from(category: AuditCategory) -> Self {
        match category {
            AuditCategory::Reload => Self::Reload,
            AuditCategory::Import => Self::Import,
            AuditCategory::Certificate => Self::Certificate,
            AuditCategory::Control => Self::Control,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuditResultDto {
    Requested,
    Succeeded,
    Failed,
    Rejected,
    Conflict,
    Partial,
    Degraded,
}

impl From<AuditResult> for AuditResultDto {
    fn from(result: AuditResult) -> Self {
        match result {
            AuditResult::Requested => Self::Requested,
            AuditResult::Succeeded => Self::Succeeded,
            AuditResult::Failed => Self::Failed,
            AuditResult::Rejected => Self::Rejected,
            AuditResult::Conflict => Self::Conflict,
            AuditResult::Partial => Self::Partial,
            AuditResult::Degraded => Self::Degraded,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct AuditStatusResponse {
    audit: AuditStatusDto,
}

impl From<AuditStatus> for AuditStatusResponse {
    fn from(status: AuditStatus) -> Self {
        Self {
            audit: status.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditStatusDto {
    state: AuditComponentStateDto,
    persistent: bool,
    degraded: bool,
    record_count: u64,
    bytes: u64,
    rotated_files: u64,
    max_records: u64,
    max_record_bytes: u64,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_rotated_files: u64,
    write_failures: u64,
    corrupt_records: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<&'static str>,
}

impl From<AuditStatus> for AuditStatusDto {
    fn from(status: AuditStatus) -> Self {
        Self {
            state: status.state.into(),
            persistent: status.persistent,
            degraded: status.degraded,
            record_count: status.record_count,
            bytes: status.bytes,
            rotated_files: status.rotated_files,
            max_records: status.max_records,
            max_record_bytes: status.max_record_bytes,
            max_file_bytes: status.max_file_bytes,
            max_total_bytes: status.max_total_bytes,
            max_rotated_files: status.max_rotated_files,
            write_failures: status.write_failures,
            corrupt_records: status.corrupt_records,
            last_error: status.last_error,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuditComponentStateDto {
    Healthy,
    Degraded,
    Memory,
}

impl From<AuditComponentState> for AuditComponentStateDto {
    fn from(state: AuditComponentState) -> Self {
        match state {
            AuditComponentState::Healthy => Self::Healthy,
            AuditComponentState::Degraded => Self::Degraded,
            AuditComponentState::Memory => Self::Memory,
        }
    }
}

#[cfg(test)]
mod tests {
    use schemars::generate::SchemaSettings;
    use serde_json::json;

    use super::*;

    #[test]
    fn audit_schema_preserves_optional_field_omission_and_fixed_enums() {
        let generator = SchemaSettings::default().for_serialize().into_generator();
        let page = serde_json::to_value(generator.into_root_schema_for::<AuditPageResponse>())
            .expect("audit page schema");
        let generator = SchemaSettings::default().for_serialize().into_generator();
        let status = serde_json::to_value(generator.into_root_schema_for::<AuditStatusResponse>())
            .expect("audit status schema");

        assert_eq!(
            page["$defs"]["AuditCategoryDto"]["enum"],
            json!(["reload", "import", "certificate", "control"])
        );
        assert_eq!(
            page["$defs"]["AuditResultDto"]["enum"],
            json!([
                "requested",
                "succeeded",
                "failed",
                "rejected",
                "conflict",
                "partial",
                "degraded"
            ])
        );
        assert!(
            !page["$defs"]["AuditRecordDto"]["required"]
                .as_array()
                .expect("required")
                .contains(&json!("revision"))
        );
        assert!(
            !status["$defs"]["AuditStatusDto"]["required"]
                .as_array()
                .expect("required")
                .contains(&json!("lastError"))
        );
    }
}
