#[allow(clippy::wildcard_imports)]
use super::*;

impl RtmpManagementApi {
    pub(super) async fn vod_response(
        &self,
        route: VodRoute<'_>,
        session: &ServerSession,
    ) -> ApiResponse {
        if session.req_header().method.as_str() != "GET" {
            return ApiResponse::method_not_allowed("GET");
        }
        let active = self
            .generations
            .as_ref()
            .and_then(GenerationManager::active);
        let catalog = active.as_ref().map_or_else(
            || self.control.vod_catalog(),
            |generation| generation.rtmp_vod_catalog(),
        );
        let service = route.service.to_owned();
        let application = route.application.to_owned();
        let source = route.source.to_owned();
        let path = route.path.to_owned();
        let content_path = path.clone();
        let object = match tokio::task::spawn_blocking(move || {
            let _generation = active;
            catalog.open(&service, &application, &source, &path)
        })
        .await
        {
            Ok(Ok(object)) => object,
            Ok(Err(error)) => return vod_error_response(error),
            Err(_) => return ApiResponse::error(503, "vod_unavailable", "VOD worker failed"),
        };
        let total = object.len();
        let range_header = session
            .req_header()
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok());
        let range = match VodRange::parse(range_header, total) {
            Ok(range) => range,
            Err(VodError::InvalidRange) => {
                return ApiResponse::error(
                    416,
                    "invalid_range",
                    "the requested byte range is invalid",
                )
                .with_range(Some(format!("bytes */{total}")));
            }
            Err(error) => return vod_error_response(error),
        };
        let body = match range {
            Some(range) => match object.range(range) {
                Ok(body) => body,
                Err(error) => return vod_error_response(error),
            },
            None => Vec::new(),
        };
        let content_range = if range_header.is_some() {
            range.map(|range| format!("bytes {}-{}/{}", range.start, range.end, total))
        } else {
            None
        };
        ApiResponse::bytes(
            if range_header.is_some() { 206 } else { 200 },
            body,
            vod_content_type(&content_path),
        )
        .with_range(content_range)
    }

    pub(super) async fn media_response(
        &self,
        route: MediaRoute<'_>,
        session: &ServerSession,
    ) -> ApiResponse {
        if session.req_header().method.as_str() != "GET" {
            return ApiResponse::method_not_allowed("GET");
        }
        let active = self
            .generations
            .as_ref()
            .and_then(GenerationManager::active);
        let catalog = active.as_ref().map_or_else(
            || self.control.media_catalog(),
            |generation| generation.rtmp_media_catalog(),
        );
        let service = route.service.to_owned();
        let application = route.application.to_owned();
        let stream = route.stream.to_owned();
        let object = route.object.to_owned();
        let object = match tokio::task::spawn_blocking(move || {
            let _generation = active;
            catalog.read_object(&service, &application, &stream, &object)
        })
        .await
        {
            Ok(Ok(object)) => object,
            Ok(Err(error)) => return media_error_response(error),
            Err(_) => return ApiResponse::error(503, "media_unavailable", "media worker failed"),
        };
        let total = u64::try_from(object.body.len()).unwrap_or(u64::MAX);
        let range_header = session
            .req_header()
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok());
        let Ok(range) = VodRange::parse(range_header, total) else {
            return ApiResponse::error(
                416,
                "media_invalid_range",
                "the requested byte range is invalid",
            )
            .with_range(Some(format!("bytes */{total}")));
        };
        let body = match range {
            Some(range) => {
                let start = usize::try_from(range.start).expect("media range starts in object");
                let end = usize::try_from(range.end).expect("media range ends in object");
                object.body[start..=end].to_vec()
            }
            None => Vec::new(),
        };
        let content_range = if range_header.is_some() {
            range.map(|range| format!("bytes {}-{}/{}", range.start, range.end, total))
        } else {
            None
        };
        ApiResponse::bytes(
            if range_header.is_some() { 206 } else { 200 },
            if range_header.is_some() {
                body
            } else {
                object.body
            },
            object.content_type,
        )
        .with_range(content_range)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn vod_error_response(error: VodError) -> ApiResponse {
    match error {
        VodError::SourceNotFound | VodError::NotFound => {
            ApiResponse::error(404, "vod_not_found", "the VOD object does not exist")
        }
        VodError::InvalidPath | VodError::InvalidRange => {
            ApiResponse::error(400, "vod_invalid_path", "the VOD path or range is invalid")
        }
        VodError::SessionLimit => {
            ApiResponse::error(429, "vod_session_limit", "the VOD session limit is reached")
        }
        VodError::TooLarge => ApiResponse::error(
            413,
            "vod_too_large",
            "the VOD object exceeds its configured bound",
        ),
        VodError::RootOpen | VodError::OriginDenied | VodError::Fetch => ApiResponse::error(
            503,
            "vod_source_unavailable",
            "the VOD source is unavailable",
        ),
        VodError::InvalidFlv | VodError::InvalidMedia => ApiResponse::error(
            422,
            "vod_invalid_media",
            "the VOD object is not valid media",
        ),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn media_error_response(error: MediaStoreError) -> ApiResponse {
    match error {
        MediaStoreError::NotFound | MediaStoreError::StaleIncarnation => {
            ApiResponse::error(404, "media_not_found", "the media object does not exist")
        }
        MediaStoreError::InvalidPath => ApiResponse::error(
            400,
            "media_invalid_path",
            "the media object path is invalid",
        ),
        MediaStoreError::FileTooLarge => ApiResponse::error(
            413,
            "media_too_large",
            "the media object exceeds its configured bound",
        ),
        MediaStoreError::ManifestMalformed => ApiResponse::error(
            422,
            "media_manifest_invalid",
            "the persisted media manifest is malformed",
        ),
        MediaStoreError::Read(_) => {
            ApiResponse::error(503, "media_unavailable", "the media object cannot be read")
        }
        MediaStoreError::RootOpen(_)
        | MediaStoreError::RootNotExclusive
        | MediaStoreError::RootScan(_)
        | MediaStoreError::ExistingUsageExceedsQuota
        | MediaStoreError::ActiveStreamLimit
        | MediaStoreError::Quota
        | MediaStoreError::Publish(_)
        | MediaStoreError::Cleanup(_) => {
            ApiResponse::error(503, "media_unavailable", "the media store is unavailable")
        }
    }
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn vod_content_type(path: &str) -> &'static str {
    if path.ends_with(".mp4") {
        "video/mp4"
    } else if path.ends_with(".m4v") {
        "video/x-m4v"
    } else {
        "video/x-flv"
    }
}
