use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::Read as _,
    os::unix::ffi::OsStringExt as _,
    path::{Component, Path},
    sync::Arc,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};
use openssl::{memcmp, sha::sha256};
use oxiroute_config::{
    HttpAccessPolicy, HttpCookiePathRewrite, HttpLiteralHeader, HttpProxyPolicy,
    HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryTrigger, HttpUpstreamHost,
};
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, FileType, Mode, OFlags},
    io::Errno,
};
use zeroize::Zeroizing;

use crate::upstream_peer::UpstreamPlan;

const MIN_ACCESS_TOKEN_BYTES: usize = 32;
const MAX_ACCESS_TOKEN_BYTES: usize = 512;
const MAX_ACCESS_TOKEN_FILE_BYTES: usize = MAX_ACCESS_TOKEN_BYTES + 2;
pub(crate) const MAX_STATIC_FILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct HttpRoutePlan {
    pub(crate) access: Option<BearerTokenAccess>,
    pub(crate) action: HttpActionPlan,
}

#[derive(Debug)]
pub(crate) enum HttpActionPlan {
    Proxy(ProxyActionPlan),
    Fixed(FixedResponsePlan),
    Redirect(RedirectPlan),
    Static(StaticFilesPlan),
}

#[derive(Debug)]
pub(crate) struct ProxyActionPlan {
    pub(crate) pool: Arc<UpstreamPlan>,
    pub(crate) policy: ProxyPolicyPlan,
}

#[derive(Debug)]
pub(crate) struct ProxyPolicyPlan {
    pub(crate) upstream_host: HttpUpstreamHost,
    pub(crate) request_headers: Box<[RequestHeaderMutationPlan]>,
    pub(crate) response_headers: Box<[ResponseHeaderMutationPlan]>,
    pub(crate) cookie_path_rewrites: Box<[HttpCookiePathRewrite]>,
    pub(crate) max_retries: u8,
    pub(crate) retry_triggers: Box<[HttpRetryTrigger]>,
}

impl ProxyPolicyPlan {
    pub(crate) fn compile(policy: &HttpProxyPolicy) -> Self {
        Self {
            upstream_host: policy.upstream_host.clone(),
            request_headers: policy
                .request_headers
                .iter()
                .map(RequestHeaderMutationPlan::compile)
                .collect(),
            response_headers: policy
                .response_headers
                .iter()
                .map(ResponseHeaderMutationPlan::compile)
                .collect(),
            cookie_path_rewrites: policy
                .response_cookie_path_rewrites
                .clone()
                .into_boxed_slice(),
            max_retries: policy.retry.max_retries,
            retry_triggers: policy.retry.triggers.clone().into_boxed_slice(),
        }
    }

    pub(crate) fn retries_on(&self, trigger: HttpRetryTrigger) -> bool {
        self.retry_triggers.contains(&trigger)
    }
}

#[derive(Debug)]
pub(crate) enum RequestHeaderMutationPlan {
    Set {
        name: HeaderName,
        value: RequestHeaderValuePlan,
    },
    Remove {
        name: HeaderName,
    },
}

impl RequestHeaderMutationPlan {
    fn compile(mutation: &HttpRequestHeaderMutation) -> Self {
        match mutation {
            HttpRequestHeaderMutation::Set { name, value } => Self::Set {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated request header name"),
                value: RequestHeaderValuePlan::compile(value),
            },
            HttpRequestHeaderMutation::Remove { name } => Self::Remove {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated request header name"),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum RequestHeaderValuePlan {
    Literal(HeaderValue),
    IncomingAuthority,
    NormalizedHost,
    ClientIp,
    SelectedUpstreamHost,
}

impl RequestHeaderValuePlan {
    fn compile(value: &HttpRequestHeaderValue) -> Self {
        match value {
            HttpRequestHeaderValue::Literal { value } => {
                Self::Literal(HeaderValue::from_str(value).expect("validated request header value"))
            }
            HttpRequestHeaderValue::IncomingAuthority => Self::IncomingAuthority,
            HttpRequestHeaderValue::NormalizedHost => Self::NormalizedHost,
            HttpRequestHeaderValue::ClientIp => Self::ClientIp,
            HttpRequestHeaderValue::SelectedUpstreamHost => Self::SelectedUpstreamHost,
        }
    }
}

#[derive(Debug)]
pub(crate) enum ResponseHeaderMutationPlan {
    Set {
        name: HeaderName,
        value: HeaderValue,
    },
    Remove {
        name: HeaderName,
    },
}

impl ResponseHeaderMutationPlan {
    fn compile(mutation: &HttpResponseHeaderMutation) -> Self {
        match mutation {
            HttpResponseHeaderMutation::Set { name, value } => Self::Set {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
                value: HeaderValue::from_str(value).expect("validated response header value"),
            },
            HttpResponseHeaderMutation::Remove { name } => Self::Remove {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct FixedResponsePlan {
    pub(crate) status: u16,
    pub(crate) body: Bytes,
    pub(crate) headers: Box<[(HeaderName, HeaderValue)]>,
}

impl FixedResponsePlan {
    pub(crate) fn compile(status: u16, body: &str, headers: &[HttpLiteralHeader]) -> Self {
        Self {
            status,
            body: Bytes::copy_from_slice(body.as_bytes()),
            headers: headers
                .iter()
                .map(|header| {
                    (
                        HeaderName::from_bytes(header.name.as_bytes())
                            .expect("validated fixed-response header name"),
                        HeaderValue::from_str(&header.value)
                            .expect("validated fixed-response header value"),
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RedirectPlan {
    pub(crate) status: u16,
    pub(crate) location: HttpRedirectLocation,
}

pub(crate) struct BearerTokenAccess {
    digest: [u8; 32],
    header_name: HeaderName,
    challenge: HeaderValue,
}

impl std::fmt::Debug for BearerTokenAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BearerTokenAccess")
            .field("header_name", &self.header_name)
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl BearerTokenAccess {
    pub(crate) fn load(policy: &HttpAccessPolicy) -> Result<Self, AccessPreflightError> {
        let HttpAccessPolicy::BearerTokenFile {
            token_file_path,
            header_name,
            realm,
        } = policy;
        let descriptor = rustix_fs::open(
            token_file_path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| AccessPreflightError)?;
        let before = rustix_fs::fstat(&descriptor).map_err(|_| AccessPreflightError)?;
        if !FileType::from_raw_mode(before.st_mode).is_file()
            || !matches!(before.st_mode & 0o7777, 0o400 | 0o600)
        {
            return Err(AccessPreflightError);
        }
        let size = usize::try_from(before.st_size).map_err(|_| AccessPreflightError)?;
        if size > MAX_ACCESS_TOKEN_FILE_BYTES {
            return Err(AccessPreflightError);
        }
        let mut file = File::from(descriptor);
        let mut token = Zeroizing::new(Vec::with_capacity(size));
        file.by_ref()
            .take(u64::try_from(MAX_ACCESS_TOKEN_FILE_BYTES + 1).expect("token bound fits u64"))
            .read_to_end(&mut token)
            .map_err(|_| AccessPreflightError)?;
        let after = rustix_fs::fstat(&file).map_err(|_| AccessPreflightError)?;
        if token.len() > MAX_ACCESS_TOKEN_FILE_BYTES || !same_file_snapshot(&before, &after) {
            return Err(AccessPreflightError);
        }
        trim_one_line_ending(&mut token);
        if !(MIN_ACCESS_TOKEN_BYTES..=MAX_ACCESS_TOKEN_BYTES).contains(&token.len())
            || !token.iter().all(|byte| matches!(byte, 0x21..=0x7e))
        {
            return Err(AccessPreflightError);
        }
        let challenge = realm.as_ref().map_or_else(
            || HeaderValue::from_static("Bearer"),
            |realm| {
                HeaderValue::from_str(&format!("Bearer realm=\"{realm}\""))
                    .expect("validated Bearer realm")
            },
        );
        Ok(Self {
            digest: sha256(&token),
            header_name: HeaderName::from_bytes(header_name.as_bytes())
                .expect("validated access header name"),
            challenge,
        })
    }

    pub(crate) fn authorizes(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(&self.header_name).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        value
            .as_bytes()
            .strip_prefix(b"Bearer ")
            .is_some_and(|candidate| memcmp::eq(&self.digest, &sha256(candidate)))
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        &self.challenge
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("HTTP route access policy failed secure preflight")]
pub(crate) struct AccessPreflightError;

#[derive(Debug)]
pub(crate) struct StaticFilesPlan {
    root: Arc<OwnedFd>,
    indexes: Box<[OsString]>,
    fallback: Option<Box<[OsString]>>,
}

impl StaticFilesPlan {
    pub(crate) fn open(
        root: &Path,
        indexes: &[String],
        fallback: Option<&Path>,
    ) -> Result<Self, StaticPreflightError> {
        Ok(Self {
            root: Arc::new(open_pinned_directory(root).map_err(|_| StaticPreflightError)?),
            indexes: indexes.iter().map(OsString::from).collect(),
            fallback: fallback
                .map(path_components)
                .transpose()
                .map_err(|()| StaticPreflightError)?
                .map(Vec::into_boxed_slice),
        })
    }

    pub(crate) async fn serve(&self, request_path: &str) -> Result<StaticFile, StaticServeError> {
        let components = request_components(request_path)?;
        let root = Arc::clone(&self.root);
        let indexes = self.indexes.clone();
        let fallback = self.fallback.clone();
        tokio::task::spawn_blocking(move || {
            match read_static_target(&root, &components, &indexes) {
                Err(StaticServeError::NotFound) if fallback.is_some() => {
                    read_static_target(&root, fallback.as_deref().expect("checked fallback"), &[])
                }
                result => result,
            }
        })
        .await
        .map_err(|_| StaticServeError::Unavailable)?
    }
}

#[derive(Debug)]
pub(crate) struct StaticFile {
    pub(crate) body: Bytes,
    pub(crate) content_type: &'static str,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("HTTP static root failed secure preflight")]
pub(crate) struct StaticPreflightError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StaticServeError {
    #[error("static target was not found")]
    NotFound,
    #[error("static target is not safely servable")]
    Unsafe,
    #[error("static target exceeds the serving bound")]
    TooLarge,
    #[error("static target could not be read")]
    Unavailable,
}

fn open_pinned_directory(path: &Path) -> Result<OwnedFd, rustix::io::Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory = rustix_fs::open(Path::new("/"), flags, Mode::empty())?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = rustix_fs::openat(&directory, name, flags, Mode::empty())?;
            }
            Component::ParentDir | Component::Prefix(_) => return Err(rustix::io::Errno::INVAL),
        }
    }
    Ok(directory)
}

fn read_static_target(
    root: &OwnedFd,
    components: &[OsString],
    indexes: &[OsString],
) -> Result<StaticFile, StaticServeError> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory =
        rustix::io::fcntl_dupfd_cloexec(root, 0).map_err(|_| StaticServeError::Unavailable)?;
    let Some((file_name, parents)) = components.split_last() else {
        return read_index(&directory, indexes);
    };
    for parent in parents {
        directory = rustix_fs::openat(&directory, parent, directory_flags, Mode::empty())
            .map_err(static_open_error)?;
    }
    let descriptor = rustix_fs::openat(
        &directory,
        file_name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(static_open_error)?;
    let metadata = rustix_fs::fstat(&descriptor).map_err(|_| StaticServeError::Unavailable)?;
    if FileType::from_raw_mode(metadata.st_mode).is_dir() {
        return read_index(&descriptor, indexes);
    }
    read_regular_file(descriptor, file_name)
}

fn read_index(directory: &OwnedFd, indexes: &[OsString]) -> Result<StaticFile, StaticServeError> {
    for index in indexes {
        let descriptor = match rustix_fs::openat(
            directory,
            index,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(static_open_error(error)),
        };
        match read_regular_file(descriptor, index) {
            Err(StaticServeError::NotFound) => {}
            result => return result,
        }
    }
    Err(StaticServeError::NotFound)
}

fn static_open_error(error: Errno) -> StaticServeError {
    match error {
        Errno::NOENT => StaticServeError::NotFound,
        Errno::LOOP | Errno::NOTDIR | Errno::ACCESS | Errno::PERM => StaticServeError::Unsafe,
        _ => StaticServeError::Unavailable,
    }
}

fn read_regular_file(descriptor: OwnedFd, name: &OsStr) -> Result<StaticFile, StaticServeError> {
    let before = rustix_fs::fstat(&descriptor).map_err(|_| StaticServeError::Unavailable)?;
    if !FileType::from_raw_mode(before.st_mode).is_file() {
        return Err(StaticServeError::Unsafe);
    }
    let size = usize::try_from(before.st_size).map_err(|_| StaticServeError::TooLarge)?;
    if size > MAX_STATIC_FILE_BYTES {
        return Err(StaticServeError::TooLarge);
    }
    let mut file = File::from(descriptor);
    let mut body = Vec::with_capacity(size);
    file.by_ref()
        .take(u64::try_from(MAX_STATIC_FILE_BYTES + 1).expect("static bound fits u64"))
        .read_to_end(&mut body)
        .map_err(|_| StaticServeError::Unavailable)?;
    let after = rustix_fs::fstat(&file).map_err(|_| StaticServeError::Unavailable)?;
    if body.len() > MAX_STATIC_FILE_BYTES
        || body.len() != size
        || !same_file_snapshot(&before, &after)
    {
        return Err(StaticServeError::Unavailable);
    }
    Ok(StaticFile {
        body: Bytes::from(body),
        content_type: content_type(name),
    })
}

fn same_file_snapshot(first: &rustix_fs::Stat, second: &rustix_fs::Stat) -> bool {
    first.st_dev == second.st_dev
        && first.st_ino == second.st_ino
        && first.st_mode == second.st_mode
        && first.st_size == second.st_size
        && first.st_mtime == second.st_mtime
        && first.st_mtime_nsec == second.st_mtime_nsec
        && first.st_ctime == second.st_ctime
        && first.st_ctime_nsec == second.st_ctime_nsec
}

fn request_components(path: &str) -> Result<Vec<OsString>, StaticServeError> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .map(|component| {
            let decoded = percent_decode(component.as_bytes())?;
            if decoded.is_empty()
                || decoded.as_slice() == b"."
                || decoded.as_slice() == b".."
                || decoded.contains(&0)
                || decoded.contains(&b'/')
                || decoded.contains(&b'\\')
            {
                return Err(StaticServeError::Unsafe);
            }
            Ok(OsString::from_vec(decoded))
        })
        .collect()
}

fn path_components(path: &Path) -> Result<Vec<OsString>, ()> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::CurDir if path.as_os_str().is_empty() => {}
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => return Err(()),
        }
    }
    Ok(components)
}

fn percent_decode(value: &[u8]) -> Result<Vec<u8>, StaticServeError> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'%' {
            let digits = value
                .get(index + 1..index + 3)
                .ok_or(StaticServeError::Unsafe)?;
            decoded.push(
                hex(digits[0])
                    .and_then(|high| hex(digits[1]).map(|low| high << 4 | low))
                    .ok_or(StaticServeError::Unsafe)?,
            );
            index += 3;
        } else {
            decoded.push(value[index]);
            index += 1;
        }
    }
    Ok(decoded)
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn content_type(name: &OsStr) -> &'static str {
    let extension = Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    match extension.to_ascii_lowercase().as_str() {
        "css" => "text/css; charset=utf-8",
        "gif" => "image/gif",
        "htm" | "html" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

fn trim_one_line_ending(bytes: &mut Vec<u8>) {
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
}
