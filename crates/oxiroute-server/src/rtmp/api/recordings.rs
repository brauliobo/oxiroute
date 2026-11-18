use std::{fs::File, sync::Arc};

use oxiroute_rtmp::{
    CatalogRecording, RecordingCatalog, RecordingCatalogError, RecordingStatus,
    RecordingStoreError, VodRange,
};

use super::{
    ApiResponse,
    dto::{RecordingDeleteResponse, RecordingDto, RecordingListResponse},
};

const DEFAULT_LIST_LIMIT: usize = 100;
const MAX_LIST_LIMIT: usize = 1_000;
const RECORDING_CONTENT_TYPE: &str = "video/x-flv";

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    List,
    Item(&'a str),
    Content(&'a str),
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    if path == "/api/v1/rtmp/recordings" {
        return Some(Route::List);
    }
    let mut segments = path.strip_prefix("/api/v1/rtmp/recordings/")?.split('/');
    let id = segments.next().filter(|id| !id.is_empty())?;
    match (segments.next(), segments.next()) {
        (None, None) => Some(Route::Item(id)),
        (Some("content"), None) => Some(Route::Content(id)),
        _ => None,
    }
}

pub(super) struct RecordingStream {
    pub(super) file: Arc<File>,
    pub(super) start: u64,
    pub(super) length: u64,
    pub(super) total: u64,
    pub(super) partial: bool,
    pub(super) filename: String,
}

pub(super) enum ContentOutcome {
    Reply(ApiResponse),
    Stream(RecordingStream),
}

pub(super) fn handle(
    route: Route<'_>,
    method: &str,
    catalog: &RecordingCatalog,
    query: Option<&str>,
) -> ApiResponse {
    match (route, method) {
        (Route::List, "GET") => list(catalog, query),
        (Route::Item(id), "GET") => catalog.get(id).map_or_else(error_response, |recording| {
            ApiResponse::dto(200, &RecordingDto::from(&recording))
        }),
        (Route::Item(id), "DELETE") => {
            catalog.delete(id).map_or_else(error_response, |recording| {
                ApiResponse::dto(200, &RecordingDeleteResponse::project(&recording))
            })
        }
        (Route::Item(_), _) => ApiResponse::method_not_allowed("GET, DELETE"),
        (Route::List | Route::Content(_), _) => ApiResponse::method_not_allowed("GET"),
    }
}

pub(super) fn content(
    catalog: &RecordingCatalog,
    id: &str,
    range_header: Option<&str>,
) -> ContentOutcome {
    let (recording, file, total) = match catalog.open(id) {
        Ok(opened) => opened,
        Err(error) => return ContentOutcome::Reply(error_response(error)),
    };
    let Ok(range) = VodRange::parse(range_header, total) else {
        return ContentOutcome::Reply(
            ApiResponse::error(416, "invalid_range", "the requested byte range is invalid")
                .with_range(Some(format!("bytes */{total}"))),
        );
    };
    let (start, length) = range.map_or((0, 0), |range| (range.start, range.length()));
    ContentOutcome::Stream(RecordingStream {
        file: Arc::new(file),
        start,
        length,
        total,
        partial: range_header.is_some(),
        filename: download_name(&recording),
    })
}

pub(super) const fn content_type() -> &'static str {
    RECORDING_CONTENT_TYPE
}

fn download_name(recording: &CatalogRecording) -> String {
    recording
        .entry
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn list(catalog: &RecordingCatalog, query: Option<&str>) -> ApiResponse {
    let filter = match ListFilter::parse(query) {
        Ok(filter) => filter,
        Err(message) => return ApiResponse::error(400, "invalid_recording_query", message),
    };
    let recordings = match catalog.list() {
        Ok(recordings) => recordings,
        Err(error) => return error_response(error),
    };
    let mut selected = recordings
        .into_iter()
        .filter(|recording| filter.accepts(recording))
        .collect::<Vec<_>>();
    let more = selected.len() > filter.limit;
    selected.truncate(filter.limit);
    let next_cursor = more
        .then(|| selected.last())
        .flatten()
        .map(|last| format!("{}.{}", last.entry.modified_unix_ms, last.entry.id));
    ApiResponse::dto(
        200,
        &RecordingListResponse::project(&selected, next_cursor, &catalog.roots()),
    )
}

struct ListFilter {
    stream: Option<String>,
    recorder: Option<String>,
    status: Option<RecordingStatus>,
    cursor: Option<(u64, String)>,
    limit: usize,
}

impl ListFilter {
    fn parse(query: Option<&str>) -> Result<Self, &'static str> {
        let mut filter = Self {
            stream: None,
            recorder: None,
            status: None,
            cursor: None,
            limit: DEFAULT_LIST_LIMIT,
        };
        for pair in query
            .unwrap_or_default()
            .split('&')
            .filter(|pair| !pair.is_empty())
        {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value).ok_or("query value is not valid percent-encoding")?;
            match key {
                "stream" => filter.stream = Some(value),
                "recorder" => filter.recorder = Some(value),
                "status" => {
                    filter.status = Some(match value.as_str() {
                        "recording" => RecordingStatus::Recording,
                        "complete" => RecordingStatus::Complete,
                        "partial" => RecordingStatus::Partial,
                        _ => return Err("status must be recording, complete or partial"),
                    });
                }
                "limit" => {
                    filter.limit = value
                        .parse::<usize>()
                        .ok()
                        .filter(|limit| (1..=MAX_LIST_LIMIT).contains(limit))
                        .ok_or("limit must be between 1 and 1000")?;
                }
                "cursor" => {
                    let (modified, id) = value.split_once('.').ok_or("cursor is invalid")?;
                    filter.cursor = Some((
                        modified.parse().map_err(|_| "cursor is invalid")?,
                        id.to_owned(),
                    ));
                }
                _ => return Err("unknown query parameter"),
            }
        }
        Ok(filter)
    }

    fn accepts(&self, recording: &CatalogRecording) -> bool {
        let entry = &recording.entry;
        self.stream
            .as_ref()
            .is_none_or(|stream| entry.stream.as_ref() == Some(stream))
            && self.status.is_none_or(|status| entry.status == status)
            && self.recorder.as_ref().is_none_or(|name| {
                recording
                    .recorders
                    .iter()
                    .any(|source| &source.recorder == name)
            })
            && self
                .cursor
                .as_ref()
                .is_none_or(|(modified, id)| (entry.modified_unix_ms, &entry.id) < (*modified, id))
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let digits = value.get(index + 1..index + 3)?;
                decoded.push(u8::from_str_radix(digits, 16).ok()?);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

#[allow(clippy::needless_pass_by_value)]
fn error_response(error: RecordingCatalogError) -> ApiResponse {
    match error {
        RecordingCatalogError::NotFound
        | RecordingCatalogError::Store(RecordingStoreError::InvalidRelativeName) => {
            ApiResponse::error(404, "recording_not_found", "the recording does not exist")
        }
        RecordingCatalogError::Active => ApiResponse::error(
            409,
            "recording_active",
            "the recording is being written and cannot be deleted",
        ),
        RecordingCatalogError::Store(_) => ApiResponse::error(
            503,
            "recording_storage_unavailable",
            "the recording storage is unavailable",
        ),
    }
}
