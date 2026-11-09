#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct StaticFilesPlan {
    pub(super) root: Arc<OwnedFd>,
    pub(super) directory_policy: StaticDirectoryPolicy,
    pub(super) fallback: Option<Box<[OsString]>>,
    pub(super) mapping: HttpStaticPathMapping,
    pub(super) mount_path: String,
    pub(super) directory_redirects: bool,
    pub(super) try_files: Box<[StaticTryFilePlan]>,
    pub(super) etag: bool,
    pub(super) mime: HashMap<String, HeaderValue>,
    pub(super) default_type: Option<HeaderValue>,
    pub(super) headers: Box<[(HeaderName, HeaderValue, bool)]>,
    pub(super) error_responses: HashMap<u16, StaticErrorResponsePlan>,
    pub(super) cache: StaticCache,
}

#[derive(Clone, Debug)]
pub(crate) struct StaticFilesBlueprint {
    pub(crate) root_directory: std::path::PathBuf,
    directory_policy: StaticDirectoryPolicy,
    fallback: Option<Box<[OsString]>>,
    mapping: HttpStaticPathMapping,
    mount_path: String,
    directory_redirects: bool,
    try_files: Box<[StaticTryFilePlan]>,
    etag: bool,
    mime: HashMap<String, HeaderValue>,
    default_type: Option<HeaderValue>,
    headers: Box<[(HeaderName, HeaderValue, bool)]>,
    error_responses: HashMap<u16, StaticErrorResponsePlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StaticRequestDecision {
    NotModified,
    PreconditionFailed,
    Serve { range: Option<(u64, u64)> },
    RangeNotSatisfiable,
}

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "directory listing, timestamp, size, and nginx index behavior are independent policies"
)]
pub(crate) struct StaticDirectoryPolicy {
    pub(super) indexes: Box<[OsString]>,
    pub(super) autoindex: bool,
    pub(super) exact_size: bool,
    pub(super) local_time: bool,
    pub(super) internal_index_redirects: bool,
}

impl StaticDirectoryPolicy {
    pub(super) fn disabled() -> Self {
        Self {
            indexes: Box::new([]),
            autoindex: false,
            exact_size: true,
            local_time: false,
            internal_index_redirects: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum StaticTryFilePlan {
    RequestPath,
    RequestPathDirectory,
    Relative(Box<[OsString]>),
    Status(u16),
}

#[derive(Clone, Debug)]
pub(crate) enum StaticErrorResponsePlan {
    File {
        components: Box<[OsString]>,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    InternalRedirect {
        path: String,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    Literal {
        body: Bytes,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
}

impl StaticFilesPlan {
    pub(crate) fn acquire(blueprint: &StaticFilesBlueprint) -> Result<Self, StaticPreflightError> {
        Ok(Self {
            root: Arc::new(
                open_pinned_directory(&blueprint.root_directory)
                    .map_err(|_| StaticPreflightError)?,
            ),
            directory_policy: blueprint.directory_policy.clone(),
            fallback: blueprint.fallback.clone(),
            mapping: blueprint.mapping,
            mount_path: blueprint.mount_path.clone(),
            directory_redirects: blueprint.directory_redirects,
            try_files: blueprint.try_files.clone(),
            etag: blueprint.etag,
            mime: blueprint.mime.clone(),
            default_type: blueprint.default_type.clone(),
            headers: blueprint.headers.clone(),
            error_responses: blueprint.error_responses.clone(),
            cache: StaticCache::default(),
        })
    }
}

impl StaticFilesBlueprint {
    #[expect(
        clippy::too_many_lines,
        reason = "the static action is normalized as one immutable policy"
    )]
    pub(crate) fn compile(
        mount_path: &str,
        action: &oxiroute_config::HttpRouteAction,
    ) -> Result<Self, StaticPreflightError> {
        let oxiroute_config::HttpRouteAction::StaticFiles {
            root_directory,
            path_mapping,
            index_files,
            internal_index_redirects,
            directory_redirects,
            spa_fallback,
            try_files,
            autoindex,
            autoindex_exact_size,
            autoindex_local_time,
            etag,
            mime,
            headers,
            error_responses,
        } = action
        else {
            return Err(StaticPreflightError);
        };
        let default_type = mime
            .default_type
            .as_deref()
            .map(HeaderValue::from_str)
            .transpose()
            .map_err(|_| StaticPreflightError)?;
        let mime = mime
            .types
            .iter()
            .map(|entry| {
                Ok((
                    entry.extension.clone(),
                    HeaderValue::from_str(&entry.content_type).map_err(|_| StaticPreflightError)?,
                ))
            })
            .collect::<Result<HashMap<_, _>, StaticPreflightError>>()?;
        let try_files = try_files
            .iter()
            .map(|candidate| match candidate {
                HttpStaticTryFile::RequestPath => Ok(StaticTryFilePlan::RequestPath),
                HttpStaticTryFile::RequestPathDirectory => {
                    Ok(StaticTryFilePlan::RequestPathDirectory)
                }
                HttpStaticTryFile::Relative { path } => path_components(path)
                    .map(Vec::into_boxed_slice)
                    .map(StaticTryFilePlan::Relative)
                    .map_err(|()| StaticPreflightError),
                HttpStaticTryFile::Status { status } => Ok(StaticTryFilePlan::Status(*status)),
            })
            .collect::<Result<Box<[_]>, _>>()?;
        let error_responses = error_responses
            .iter()
            .flat_map(|response| {
                response
                    .statuses
                    .iter()
                    .map(move |status| (*status, response))
            })
            .map(|(status, response)| {
                let headers = response
                    .headers
                    .iter()
                    .map(|header| {
                        Ok((
                            HeaderName::from_bytes(header.name.as_bytes())
                                .map_err(|_| StaticPreflightError)?,
                            HeaderValue::from_str(&header.value)
                                .map_err(|_| StaticPreflightError)?,
                        ))
                    })
                    .collect::<Result<Box<[_]>, StaticPreflightError>>()?;
                if let Some(body) = &response.body {
                    return Ok((
                        status,
                        StaticErrorResponsePlan::Literal {
                            body: Bytes::copy_from_slice(body.as_bytes()),
                            headers,
                        },
                    ));
                }
                if let Some(path) = &response.internal_redirect {
                    return Ok((
                        status,
                        StaticErrorResponsePlan::InternalRedirect {
                            path: path.clone(),
                            headers,
                        },
                    ));
                }
                path_components(response.file.as_deref().ok_or(StaticPreflightError)?)
                    .map(|components| {
                        (
                            status,
                            StaticErrorResponsePlan::File {
                                components: components.into_boxed_slice(),
                                headers,
                            },
                        )
                    })
                    .map_err(|()| StaticPreflightError)
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(Self {
            root_directory: root_directory.clone(),
            directory_policy: StaticDirectoryPolicy {
                indexes: index_files.iter().map(OsString::from).collect(),
                autoindex: *autoindex,
                exact_size: *autoindex_exact_size,
                local_time: *autoindex_local_time,
                internal_index_redirects: *internal_index_redirects,
            },
            fallback: spa_fallback
                .as_deref()
                .map(path_components)
                .transpose()
                .map_err(|()| StaticPreflightError)?
                .map(Vec::into_boxed_slice),
            mapping: *path_mapping,
            mount_path: mount_path.to_owned(),
            directory_redirects: *directory_redirects,
            try_files,
            etag: *etag,
            mime,
            default_type,
            headers: headers
                .iter()
                .map(|header| {
                    Ok((
                        HeaderName::from_bytes(header.name.as_bytes())
                            .map_err(|_| StaticPreflightError)?,
                        HeaderValue::from_str(&header.value).map_err(|_| StaticPreflightError)?,
                        header.always,
                    ))
                })
                .collect::<Result<Box<[_]>, StaticPreflightError>>()?,
            error_responses,
        })
    }
}

impl StaticFilesPlan {
    pub(crate) async fn serve(&self, request_path: &str) -> Result<StaticTarget, StaticServeError> {
        let now = Instant::now();
        if let Some(file) = self.cache.get(request_path, now) {
            return Ok(StaticTarget::File(file));
        }
        let lookup = StaticLookup {
            components: self.request_components(request_path)?,
            root: Arc::clone(&self.root),
            directory_policy: self.directory_policy.clone(),
            fallback: self.fallback.clone(),
            try_files: self.try_files.clone(),
            directory_redirects: self.directory_redirects,
            request_has_trailing_slash: request_path.ends_with('/'),
            request_path: request_path.to_owned(),
        };
        let target =
            tokio::task::spawn_blocking(move || lookup.find().and_then(StaticTarget::load))
                .await
                .map_err(|_| StaticServeError::Unavailable)??;
        let StaticTarget::File(file) = target else {
            return Ok(target);
        };
        let file = self.typed(file);
        self.cache.insert(request_path, &file, now);
        Ok(StaticTarget::File(file))
    }

    /// The configured MIME mapping decides the content type; the lookup only knows the built-in one.
    fn typed(&self, mut file: StaticFile) -> StaticFile {
        file.content_type = self.content_type(&file.name);
        file
    }

    pub(crate) async fn error_document(&self, status: u16) -> Option<StaticErrorTarget> {
        let response = self.error_responses.get(&status)?.clone();
        let components = match response {
            StaticErrorResponsePlan::InternalRedirect { path, headers } => {
                return Some(StaticErrorTarget::InternalRedirect { path, headers });
            }
            StaticErrorResponsePlan::Literal { body, headers } => {
                return Some(StaticErrorTarget::Literal { body, headers });
            }
            StaticErrorResponsePlan::File {
                components,
                headers,
            } => (components, headers),
        };
        let (components, headers) = components;
        let root = Arc::clone(&self.root);
        let file = tokio::task::spawn_blocking(move || {
            read_static_target(
                &root,
                &components,
                &StaticDirectoryPolicy::disabled(),
                false,
                false,
                "",
            )
            .and_then(StaticTarget::load)
            .ok()
            .and_then(|target| match target {
                StaticTarget::File(file) => Some(file),
                StaticTarget::Autoindex { .. }
                | StaticTarget::DirectoryRedirect { .. }
                | StaticTarget::InternalRedirect { .. }
                | StaticTarget::Status(_) => None,
            })
        })
        .await
        .ok()
        .flatten()?;
        Some(StaticErrorTarget::File {
            file: self.typed(file),
            headers,
        })
    }

    pub(crate) fn headers(&self, status: u16) -> Vec<(HeaderName, HeaderValue)> {
        self.headers
            .iter()
            .filter(|(_, _, always)| *always || nginx_add_header_status(status))
            .map(|(name, value, _)| (name.clone(), value.clone()))
            .collect()
    }

    pub(crate) fn content_type(&self, name: &OsStr) -> HeaderValue {
        let file_name = name.to_str().map(str::to_ascii_lowercase);
        file_name
            .as_ref()
            .and_then(|file_name| {
                self.mime
                    .iter()
                    .filter(|(suffix, _)| {
                        file_name.len() > suffix.len()
                            && file_name.ends_with(suffix.as_str())
                            && file_name.as_bytes()[file_name.len() - suffix.len() - 1] == b'.'
                    })
                    .max_by_key(|(suffix, _)| suffix.len())
                    .map(|(_, content_type)| content_type)
            })
            .cloned()
            .or_else(|| self.default_type.clone())
            .unwrap_or_else(|| HeaderValue::from_static(builtin_content_type(name)))
    }

    pub(crate) fn etag_enabled(&self) -> bool {
        self.etag
    }

    pub(crate) fn request_decision(
        &self,
        headers: &HeaderMap,
        file: &StaticFile,
    ) -> StaticRequestDecision {
        if let Some(value) = headers.get(IF_MATCH) {
            let matches = value.as_bytes() == b"*"
                || self.etag && etag_list_matches(value, &file.etag, false);
            if !matches {
                return StaticRequestDecision::PreconditionFailed;
            }
        } else if headers
            .get(IF_UNMODIFIED_SINCE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| httpdate::parse_http_date(value).ok())
            .is_some_and(|date| modified_after(file.modified, date))
        {
            return StaticRequestDecision::PreconditionFailed;
        }

        if let Some(value) = headers.get(IF_NONE_MATCH) {
            if value.as_bytes() == b"*" || self.etag && etag_list_matches(value, &file.etag, true) {
                return StaticRequestDecision::NotModified;
            }
        } else if headers
            .get(IF_MODIFIED_SINCE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| httpdate::parse_http_date(value).ok())
            .is_some_and(|date| !modified_after(file.modified, date))
        {
            return StaticRequestDecision::NotModified;
        }

        let apply_range = if_range_matches(headers, file, self.etag);
        match apply_range.then(|| requested_range(headers, file.size)) {
            None | Some(Ok(None)) => StaticRequestDecision::Serve { range: None },
            Some(Ok(Some(range))) => StaticRequestDecision::Serve { range: Some(range) },
            Some(Err(())) => StaticRequestDecision::RangeNotSatisfiable,
        }
    }

    fn request_components(&self, request_path: &str) -> Result<Vec<OsString>, StaticServeError> {
        let mapped = match self.mapping {
            HttpStaticPathMapping::Root => request_path,
            HttpStaticPathMapping::Alias => request_path
                .strip_prefix(&self.mount_path)
                .ok_or(StaticServeError::Unsafe)?,
        };
        request_components(mapped)
    }
}

/// What a blocking-pool thread needs to find the target of one request.
struct StaticLookup {
    components: Vec<OsString>,
    root: Arc<OwnedFd>,
    directory_policy: StaticDirectoryPolicy,
    fallback: Option<Box<[OsString]>>,
    try_files: Box<[StaticTryFilePlan]>,
    directory_redirects: bool,
    request_has_trailing_slash: bool,
    request_path: String,
}

impl StaticLookup {
    fn find(&self) -> Result<StaticTarget, StaticServeError> {
        let Self {
            components,
            root,
            directory_policy,
            fallback,
            try_files,
            directory_redirects,
            request_has_trailing_slash,
            request_path,
        } = self;
        let redirect = *directory_redirects && !request_has_trailing_slash;
        if try_files.is_empty() {
            return match read_static_target(
                root,
                components,
                directory_policy,
                false,
                redirect,
                request_path,
            ) {
                Err(StaticServeError::NotFound) if fallback.is_some() => read_static_target(
                    root,
                    fallback.as_deref().expect("checked fallback"),
                    &StaticDirectoryPolicy::disabled(),
                    false,
                    false,
                    request_path,
                ),
                result => result,
            };
        }
        for candidate in try_files {
            let result = match candidate {
                StaticTryFilePlan::RequestPath => read_static_target(
                    root,
                    components,
                    directory_policy,
                    false,
                    redirect,
                    request_path,
                ),
                StaticTryFilePlan::RequestPathDirectory => read_static_target(
                    root,
                    components,
                    directory_policy,
                    true,
                    redirect,
                    request_path,
                ),
                StaticTryFilePlan::Relative(path) => {
                    read_static_target(root, path, directory_policy, false, false, request_path)
                }
                StaticTryFilePlan::Status(status) => return Ok(StaticTarget::Status(*status)),
            };
            match result {
                Err(StaticServeError::NotFound) => {}
                result => return result,
            }
        }
        Err(StaticServeError::NotFound)
    }
}

fn requested_range(headers: &HeaderMap, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let mut values = headers.get_all(RANGE).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    let Some(value) = value.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if value.contains(',') || size == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    let range = if start.is_empty() {
        let suffix = end
            .parse::<u64>()
            .ok()
            .filter(|suffix| *suffix > 0)
            .ok_or(())?;
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start = start
            .parse::<u64>()
            .ok()
            .filter(|start| *start < size)
            .ok_or(())?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>()
                .ok()
                .filter(|end| *end >= start)
                .map(|end| end.min(size - 1))
                .ok_or(())?
        };
        (start, end)
    };
    Ok(Some(range))
}

fn if_range_matches(headers: &HeaderMap, file: &StaticFile, etag_enabled: bool) -> bool {
    let Some(value) = headers.get(IF_RANGE) else {
        return true;
    };
    if value.as_bytes().starts_with(b"\"") {
        return etag_enabled && value == file.etag;
    }
    value
        .to_str()
        .ok()
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .is_some_and(|date| !modified_after(file.modified, date))
}

fn etag_list_matches(value: &HeaderValue, etag: &HeaderValue, weak: bool) -> bool {
    let Ok(value) = value.to_str() else {
        return false;
    };
    let expected = etag.as_bytes();
    value.split(',').map(str::trim).any(|candidate| {
        let candidate = candidate.as_bytes();
        candidate == expected || weak && candidate.strip_prefix(b"W/") == Some(expected)
    })
}

fn modified_after(modified: SystemTime, date: SystemTime) -> bool {
    let modified = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let date = date
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    modified > date
}
