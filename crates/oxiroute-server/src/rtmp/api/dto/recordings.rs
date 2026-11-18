use oxiroute_rtmp::{
    CatalogRecording, RecorderSource, RecordingRetention, RecordingRootStatus, RecordingStatus,
};
use schemars::JsonSchema;
use serde::Serialize;

use super::DecimalCounter;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordingStatusDto {
    Recording,
    Complete,
    Partial,
}

impl From<RecordingStatus> for RecordingStatusDto {
    fn from(status: RecordingStatus) -> Self {
        match status {
            RecordingStatus::Recording => Self::Recording,
            RecordingStatus::Complete => Self::Complete,
            RecordingStatus::Partial => Self::Partial,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RecorderSourceDto {
    service: String,
    application: String,
    recorder: String,
}

impl From<&RecorderSource> for RecorderSourceDto {
    fn from(source: &RecorderSource) -> Self {
        Self {
            service: source.service.clone(),
            application: source.application.clone(),
            recorder: source.recorder.clone(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RecordingDto {
    id: String,
    name: String,
    status: RecordingStatusDto,
    stream: Option<String>,
    started_at_unix_ms: Option<u64>,
    ended_at_unix_ms: Option<u64>,
    duration_ms: Option<u64>,
    size_bytes: DecimalCounter,
    modified_at_unix_ms: u64,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    recorders: Vec<RecorderSourceDto>,
}

impl From<&CatalogRecording> for RecordingDto {
    fn from(recording: &CatalogRecording) -> Self {
        let entry = &recording.entry;
        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            status: entry.status.into(),
            stream: entry.stream.clone(),
            started_at_unix_ms: entry.started_unix_ms,
            ended_at_unix_ms: entry.ended_unix_ms,
            duration_ms: entry.duration_ms,
            size_bytes: entry.size_bytes.into(),
            modified_at_unix_ms: entry.modified_unix_ms,
            video_codec: entry.video_codec.clone(),
            audio_codec: entry.audio_codec.clone(),
            recorders: recording.recorders.iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordingRetentionDto {
    Reject,
    DeleteOldest,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RecordingRootDto {
    recorders: Vec<RecorderSourceDto>,
    bytes_used: DecimalCounter,
    max_bytes: Option<DecimalCounter>,
    files: u64,
    max_files: Option<u64>,
    retention: RecordingRetentionDto,
    retention_max_age_ms: Option<u64>,
    evicted_files: DecimalCounter,
    evicted_bytes: DecimalCounter,
}

impl From<&RecordingRootStatus> for RecordingRootDto {
    fn from(root: &RecordingRootStatus) -> Self {
        let RecordingRetention {
            delete_oldest,
            max_age,
        } = root.limits.retention;
        Self {
            recorders: root.recorders.iter().map(Into::into).collect(),
            bytes_used: root.stats.bytes_used.into(),
            max_bytes: root.limits.max_bytes.map(Into::into),
            files: u64::try_from(root.stats.files).unwrap_or(u64::MAX),
            max_files: root
                .limits
                .max_files
                .map(|files| u64::try_from(files).unwrap_or(u64::MAX)),
            retention: if delete_oldest {
                RecordingRetentionDto::DeleteOldest
            } else {
                RecordingRetentionDto::Reject
            },
            retention_max_age_ms: max_age
                .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
            evicted_files: root.evictions.files.into(),
            evicted_bytes: root.evictions.bytes.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RecordingListResponse {
    recordings: Vec<RecordingDto>,
    next_cursor: Option<String>,
    roots: Vec<RecordingRootDto>,
}

impl RecordingListResponse {
    pub(crate) fn project(
        recordings: &[CatalogRecording],
        next_cursor: Option<String>,
        roots: &[RecordingRootStatus],
    ) -> Self {
        Self {
            recordings: recordings.iter().map(Into::into).collect(),
            next_cursor,
            roots: roots.iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RecordingDeleteResponse {
    deleted: RecordingDto,
}

impl RecordingDeleteResponse {
    pub(crate) fn project(recording: &CatalogRecording) -> Self {
        Self {
            deleted: recording.into(),
        }
    }
}
