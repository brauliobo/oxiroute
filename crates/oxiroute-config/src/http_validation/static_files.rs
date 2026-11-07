#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct StaticFilesValidation<'a> {
    pub(super) root_directory: &'a mut std::path::PathBuf,
    pub(super) index_files: &'a [String],
    pub(super) spa_fallback: Option<&'a std::path::Path>,
    pub(super) try_files: &'a [HttpStaticTryFile],
    pub(super) mime: &'a HttpStaticMimePolicy,
    pub(super) headers: &'a mut [HttpLiteralHeader],
    pub(super) error_responses: &'a mut [HttpStaticErrorResponse],
}

pub(super) fn validate_static_files(
    service: &str,
    route_index: usize,
    fields: &mut StaticFilesValidation<'_>,
) -> Result<(), ConfigError> {
    normalize_absolute_directory(fields.root_directory).map_err(|detail| {
        invalid_route(
            service,
            route_index,
            "action.static_files.root_directory",
            detail,
        )
    })?;
    validate_static_indexes(
        service,
        route_index,
        fields.index_files,
        fields.spa_fallback,
    )?;
    validate_static_try_files(service, route_index, fields.try_files)?;
    validate_static_mime(service, route_index, fields.mime)?;
    validate_literal_headers(
        service,
        route_index,
        "action.static_files.headers",
        fields.headers,
    )?;
    validate_static_error_responses(service, route_index, fields.error_responses)
}

fn validate_static_indexes(
    service: &str,
    route_index: usize,
    index_files: &[String],
    spa_fallback: Option<&std::path::Path>,
) -> Result<(), ConfigError> {
    if index_files.len() > MAX_HTTP_STATIC_INDEX_FILES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.index_files",
            "must contain at most 8 filenames",
        ));
    }
    for index in index_files {
        let valid = !index.is_empty()
            && index.len() <= MAX_HTTP_STATIC_INDEX_BYTES
            && index.bytes().enumerate().all(|(position, byte)| {
                byte.is_ascii_alphanumeric() || (position > 0 && matches!(byte, b'.' | b'-' | b'_'))
            });
        if !valid {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.index_files",
                "each index must be a safe relative filename of at most 255 bytes",
            ));
        }
    }
    if let Some(spa_fallback) = spa_fallback {
        validate_relative_path(spa_fallback, MAX_HTTP_STATIC_FALLBACK_BYTES).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.static_files.spa_fallback",
                detail,
            )
        })?;
    }
    Ok(())
}

fn validate_static_try_files(
    service: &str,
    route_index: usize,
    try_files: &[HttpStaticTryFile],
) -> Result<(), ConfigError> {
    if try_files.len() > MAX_HTTP_STATIC_TRY_FILES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.try_files",
            "must contain at most 16 candidates",
        ));
    }
    for (candidate_index, candidate) in try_files.iter().enumerate() {
        match candidate {
            HttpStaticTryFile::Relative { path } => {
                validate_relative_path(path, MAX_HTTP_STATIC_FALLBACK_BYTES).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.static_files.try_files",
                        detail,
                    )
                })?;
            }
            HttpStaticTryFile::Status { status }
                if !(400..=599).contains(status) || candidate_index + 1 != try_files.len() =>
            {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.try_files",
                    "terminal status must be the final candidate and between 400 and 599",
                ));
            }
            HttpStaticTryFile::RequestPath
            | HttpStaticTryFile::RequestPathDirectory
            | HttpStaticTryFile::Status { .. } => {}
        }
    }
    Ok(())
}

fn validate_static_error_responses(
    service: &str,
    route_index: usize,
    error_responses: &mut [HttpStaticErrorResponse],
) -> Result<(), ConfigError> {
    if error_responses.len() > MAX_HTTP_STATIC_ERROR_RESPONSES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.error_responses",
            "must contain at most 16 responses",
        ));
    }
    let mut statuses = HashSet::new();
    for response in error_responses {
        if response.statuses.is_empty() || response.statuses.len() > MAX_HTTP_STATIC_ERROR_STATUSES
        {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.error_responses",
                "each response must contain 1..=16 statuses",
            ));
        }
        match (&response.file, &response.body) {
            (Some(file), None) => validate_relative_path(file, MAX_HTTP_STATIC_FALLBACK_BYTES)
                .map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.static_files.error_responses",
                        detail,
                    )
                })?,
            (None, Some(body)) if body.len() <= MAX_HTTP_FIXED_RESPONSE_BODY_BYTES => {}
            _ => {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses",
                    "must configure exactly one bounded file or inline body",
                ));
            }
        }
        if response.body.is_some() && response.internal_redirect.is_some() {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.error_responses.internal_redirect",
                "is valid only for a file response",
            ));
        }
        validate_literal_headers(
            service,
            route_index,
            "action.static_files.error_responses.headers",
            &mut response.headers,
        )?;
        if let Some(path) = &response.internal_redirect {
            let canonical = canonicalize_http_path(path).ok_or_else(|| {
                invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses.internal_redirect",
                    "must be an absolute canonical HTTP path",
                )
            })?;
            if canonical.as_ref() != path || path.len() > MAX_HTTP_STATIC_FALLBACK_BYTES {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses.internal_redirect",
                    "must be an absolute canonical HTTP path within the static fallback bound",
                ));
            }
        }
        for status in &response.statuses {
            if !(400..=599).contains(status) || !statuses.insert(*status) {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.static_files.error_responses",
                    "statuses must be unique values between 400 and 599",
                ));
            }
        }
    }
    Ok(())
}

fn validate_static_mime(
    service: &str,
    route_index: usize,
    mime: &HttpStaticMimePolicy,
) -> Result<(), ConfigError> {
    if mime.types.len() > MAX_HTTP_STATIC_MIME_TYPES {
        return Err(invalid_route(
            service,
            route_index,
            "action.static_files.mime.types",
            "must contain at most 2048 mappings",
        ));
    }
    if let Some(default_type) = &mime.default_type {
        validate_content_type(default_type).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.static_files.mime.default_type",
                detail,
            )
        })?;
    }
    let mut extensions = HashSet::with_capacity(mime.types.len());
    for mapping in &mime.types {
        let valid_extension = !mapping.extension.is_empty()
            && mapping.extension.len() <= MAX_HTTP_FILE_EXTENSION_BYTES
            && !mapping.extension.starts_with('.')
            && !mapping.extension.ends_with('.')
            && !mapping.extension.contains("..")
            && mapping.extension.chars().all(|character| {
                character.is_alphanumeric() || matches!(character, '+' | '-' | '_' | '.')
            });
        if !valid_extension || !extensions.insert(mapping.extension.as_str()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.static_files.mime.types",
                "extensions must be unique safe suffixes of at most 32 bytes",
            ));
        }
        validate_content_type(&mapping.content_type).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.static_files.mime.types",
                detail,
            )
        })?;
    }
    Ok(())
}

pub(super) fn validate_content_type(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_HTTP_MIME_TYPE_BYTES || !value.contains('/') {
        return Err("content type must be 1..=128 bytes and contain `/`");
    }
    validate_header_value(value)
}
