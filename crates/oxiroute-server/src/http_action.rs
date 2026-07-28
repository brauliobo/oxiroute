use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs::File,
    io::{Read as _, Write as _},
    os::unix::ffi::OsStringExt as _,
    path::{Component, Path},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, header::AUTHORIZATION};
use openssl::{memcmp, sha::sha256};
use oxiroute_config::{
    AccessLogPolicy, HttpAccessPolicy, HttpCookieAttributePolicy, HttpCookiePathRewrite,
    HttpGzipPolicy, HttpLiteralHeader, HttpProxyPolicy, HttpRedirectLocation,
    HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpRetryTarget,
    HttpRetryTrigger, HttpRouteAction, HttpRoutePolicy, HttpStaticPathMapping, HttpStaticTryFile,
    HttpUpstreamHost,
};
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags},
    io::Errno,
};
use zeroize::Zeroizing;

use crate::upstream_peer::UpstreamPlan;

const MIN_ACCESS_TOKEN_BYTES: usize = 32;
const MAX_ACCESS_TOKEN_BYTES: usize = 512;
const MAX_ACCESS_TOKEN_FILE_BYTES: usize = MAX_ACCESS_TOKEN_BYTES + 2;
const MAX_HTPASSWD_FILE_BYTES: usize = 1024 * 1024;
const MAX_BASIC_CREDENTIAL_BYTES: usize = 2048;
const MAX_CONCURRENT_BCRYPT_VERIFICATIONS: usize = 4;
const MIN_BASIC_BCRYPT_COST: u32 = 4;
const MAX_BASIC_BCRYPT_COST: u32 = 12;
pub(crate) const MAX_STATIC_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_AUTOINDEX_ENTRIES: usize = 10_000;
const MAX_AUTOINDEX_BYTES: usize = 4 * 1024 * 1024;
const ACCESS_LOG_QUEUE_CAPACITY: usize = 1_024;

#[derive(Debug)]
pub(crate) struct HttpRoutePlan {
    pub(crate) access: Option<RouteAccess>,
    pub(crate) action: HttpActionPlan,
    pub(crate) policy: RoutePolicyPlan,
    pub(crate) route_id: String,
}

#[derive(Debug)]
pub(crate) struct HttpGzipPlan {
    pub(crate) level: u32,
    pub(crate) content_types: Box<[String]>,
}

impl HttpGzipPlan {
    pub(crate) fn compile(policy: &HttpGzipPolicy) -> Self {
        Self {
            level: u32::from(policy.level),
            content_types: policy.content_types.clone().into_boxed_slice(),
        }
    }
}

pub(crate) struct AccessLog {
    sender: Option<SyncSender<Vec<u8>>>,
    service: String,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for AccessLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccessLog")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl AccessLog {
    pub(crate) fn open(
        service: &str,
        policy: Option<&AccessLogPolicy>,
    ) -> Result<Option<Self>, AccessPreflightError> {
        let Some(AccessLogPolicy::File { path }) = policy else {
            return Ok(None);
        };
        let parent = path.parent().ok_or(AccessPreflightError)?;
        let name = path.file_name().ok_or(AccessPreflightError)?;
        let parent = open_pinned_directory(parent).map_err(|_| AccessPreflightError)?;
        let descriptor = rustix_fs::openat(
            &parent,
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| AccessPreflightError)?;
        let metadata = rustix_fs::fstat(&descriptor).map_err(|_| AccessPreflightError)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(AccessPreflightError);
        }
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(ACCESS_LOG_QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name(format!("http-access-log-{service}"))
            .spawn(move || {
                let mut file = File::from(descriptor);
                while let Ok(line) = receiver.recv() {
                    if file.write_all(&line).is_err() || file.write_all(b"\n").is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| AccessPreflightError)?;
        Ok(Some(Self {
            sender: Some(sender),
            service: service.to_owned(),
            worker: Mutex::new(Some(worker)),
        }))
    }

    pub(crate) fn write(&self, event: &serde_json::Value) -> std::io::Result<()> {
        let line = serde_json::to_vec(event)?;
        self.sender
            .as_ref()
            .expect("access log sender exists until final drop")
            .try_send(line)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    std::io::Error::new(std::io::ErrorKind::WouldBlock, "access log queue is full")
                }
                TrySendError::Disconnected(_) => {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "access log writer stopped")
                }
            })
    }

    pub(crate) fn service(&self) -> &str {
        &self.service
    }
}

impl Drop for AccessLog {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RoutePolicyPlan {
    pub(crate) max_request_body_bytes: Option<u64>,
    pub(crate) connect_timeout: std::time::Duration,
    pub(crate) read_timeout: std::time::Duration,
    pub(crate) write_timeout: std::time::Duration,
}

impl RoutePolicyPlan {
    pub(crate) fn compile(policy: HttpRoutePolicy) -> Self {
        Self {
            max_request_body_bytes: policy.max_request_body_bytes,
            connect_timeout: std::time::Duration::from_millis(policy.connect_timeout_ms),
            read_timeout: std::time::Duration::from_millis(policy.read_timeout_ms),
            write_timeout: std::time::Duration::from_millis(policy.write_timeout_ms),
        }
    }

    pub(crate) fn exceeds_body_limit(self, bytes: u64) -> bool {
        self.max_request_body_bytes
            .is_some_and(|limit| bytes > limit)
    }
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
    pub(crate) cookie_attributes: Box<[HttpCookieAttributePolicy]>,
    pub(crate) max_retries: u8,
    pub(crate) retry_triggers: Box<[HttpRetryTrigger]>,
    pub(crate) retry_target: HttpRetryTarget,
    pub(crate) retry_delay: Duration,
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
            cookie_attributes: policy.response_cookie_attributes.clone().into_boxed_slice(),
            max_retries: policy.retry.max_retries,
            retry_triggers: policy.retry.triggers.clone().into_boxed_slice(),
            retry_target: policy.retry.target,
            retry_delay: Duration::from_millis(policy.retry.delay_ms),
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

    pub(crate) fn is_pingora_managed_upgrade(&self) -> bool {
        matches!(
            self,
            Self::Set {
                name,
                value: RequestHeaderValuePlan::IncomingHeader { name: source, .. },
            } if name.as_str() == "upgrade" && source.as_str() == "upgrade"
        ) || matches!(
            self,
            Self::Set {
                name,
                value: RequestHeaderValuePlan::Literal(value),
            } if name.as_str() == "connection"
                && value.as_bytes().eq_ignore_ascii_case(b"upgrade")
        )
    }
}

#[derive(Debug)]
pub(crate) enum RequestHeaderValuePlan {
    Literal(HeaderValue),
    IncomingAuthority,
    NormalizedHost,
    NginxHost {
        fallback: HeaderValue,
    },
    ClientIp,
    AppendedXForwardedFor {
        max_bytes: usize,
        except_source_cidrs: Box<[SourceCidr]>,
    },
    DownstreamScheme,
    IncomingHeader {
        name: HeaderName,
        max_bytes: usize,
    },
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
            HttpRequestHeaderValue::NginxHost { fallback } => Self::NginxHost {
                fallback: HeaderValue::from_str(fallback).expect("validated nginx host fallback"),
            },
            HttpRequestHeaderValue::ClientIp => Self::ClientIp,
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => Self::AppendedXForwardedFor {
                max_bytes: usize::try_from(*max_bytes).expect("validated header bound"),
                except_source_cidrs: except_source_cidrs
                    .iter()
                    .map(|cidr| SourceCidr::parse(cidr).expect("validated source CIDR"))
                    .collect(),
            },
            HttpRequestHeaderValue::DownstreamScheme => Self::DownstreamScheme,
            HttpRequestHeaderValue::IncomingHeader { name, max_bytes } => Self::IncomingHeader {
                name: HeaderName::from_bytes(name.as_bytes()).expect("validated incoming header"),
                max_bytes: usize::try_from(*max_bytes).expect("validated header bound"),
            },
            HttpRequestHeaderValue::SelectedUpstreamHost => Self::SelectedUpstreamHost,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SourceCidr {
    network: std::net::IpAddr,
    prefix: u8,
}

impl SourceCidr {
    fn parse(value: &str) -> Option<Self> {
        let (network, prefix) = value.split_once('/')?;
        Some(Self {
            network: network.parse().ok()?,
            prefix: prefix.parse().ok()?,
        })
    }

    pub(crate) fn contains(&self, address: std::net::IpAddr) -> bool {
        match (self.network, address) {
            (std::net::IpAddr::V4(network), std::net::IpAddr::V4(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - u32::from(self.prefix))
                };
                u32::from(network) & mask == u32::from(address) & mask
            }
            (std::net::IpAddr::V6(network), std::net::IpAddr::V6(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - u32::from(self.prefix))
                };
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

#[derive(Debug)]
pub(crate) enum ResponseHeaderMutationPlan {
    Set {
        name: HeaderName,
        value: HeaderValue,
        always: bool,
    },
    Add {
        name: HeaderName,
        value: HeaderValue,
        always: bool,
    },
    Remove {
        name: HeaderName,
    },
}

impl ResponseHeaderMutationPlan {
    fn compile(mutation: &HttpResponseHeaderMutation) -> Self {
        match mutation {
            HttpResponseHeaderMutation::Set {
                name,
                value,
                always,
            } => Self::Set {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
                value: HeaderValue::from_str(value).expect("validated response header value"),
                always: *always,
            },
            HttpResponseHeaderMutation::Add {
                name,
                value,
                always,
            } => Self::Add {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
                value: HeaderValue::from_str(value).expect("validated response header value"),
                always: *always,
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
                .filter(|header| header.always || nginx_add_header_status(status))
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
    pub(crate) headers: Box<[(HeaderName, HeaderValue)]>,
}

pub(crate) struct BearerTokenAccess {
    digest: [u8; 32],
    header_name: HeaderName,
    challenge: HeaderValue,
}

#[derive(Debug)]
pub(crate) enum RouteAccess {
    Bearer(BearerTokenAccess),
    Basic(BasicHtpasswdAccess),
}

impl RouteAccess {
    pub(crate) fn load(policy: &HttpAccessPolicy) -> Result<Self, AccessPreflightError> {
        match policy {
            HttpAccessPolicy::BearerTokenFile { .. } => {
                BearerTokenAccess::load(policy).map(Self::Bearer)
            }
            HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path,
                realm,
            } => BasicHtpasswdAccess::load(htpasswd_file_path, realm).map(Self::Basic),
        }
    }

    pub(crate) async fn authorizes(&self, headers: &HeaderMap) -> bool {
        match self {
            Self::Bearer(access) => access.authorizes(headers),
            Self::Basic(access) => access.authorizes(headers).await,
        }
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        match self {
            Self::Bearer(access) => access.challenge(),
            Self::Basic(access) => &access.challenge,
        }
    }
}

pub(crate) struct BasicHtpasswdAccess {
    challenge: HeaderValue,
    dummy_hash: String,
    users: Box<[(String, String)]>,
}

impl std::fmt::Debug for BasicHtpasswdAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BasicHtpasswdAccess")
            .field("challenge", &self.challenge)
            .field("user_count", &self.users.len())
            .finish_non_exhaustive()
    }
}

impl BasicHtpasswdAccess {
    fn load(path: &Path, realm: &str) -> Result<Self, AccessPreflightError> {
        let bytes = read_secret_file(path, MAX_HTPASSWD_FILE_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| AccessPreflightError)?;
        let mut users = Vec::new();
        let mut file_cost = None;
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (username, hash) = line.split_once(':').ok_or(AccessPreflightError)?;
            let parts = hash
                .parse::<bcrypt::HashParts>()
                .map_err(|_| AccessPreflightError)?;
            let cost = parts.get_cost();
            if username.is_empty()
                || username.len() > 256
                || username.bytes().any(|byte| byte.is_ascii_control())
                || !matches!(hash.get(..4), Some("$2y$" | "$2b$" | "$2a$"))
                || hash.len() != 60
                || !(MIN_BASIC_BCRYPT_COST..=MAX_BASIC_BCRYPT_COST).contains(&cost)
                || file_cost.is_some_and(|existing| existing != cost)
                || users.iter().any(|(existing, _)| existing == username)
            {
                return Err(AccessPreflightError);
            }
            file_cost = Some(cost);
            users.push((username.to_owned(), hash.to_owned()));
        }
        if users.is_empty() {
            return Err(AccessPreflightError);
        }
        let cost = file_cost.expect("nonempty bcrypt file has a cost");
        let dummy_hash =
            bcrypt::hash_with_salt(b"oxiroute-unknown-basic-user", cost, *b"OxiRouteDummy123")
                .map_err(|_| AccessPreflightError)?
                .to_string();
        Ok(Self {
            challenge: HeaderValue::from_str(&format!(
                "Basic realm=\"{realm}\", charset=\"UTF-8\""
            ))
            .expect("validated Basic realm"),
            dummy_hash,
            users: users.into_boxed_slice(),
        })
    }

    async fn authorizes(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        let bytes = value.as_bytes();
        let Some(encoded) = bytes.get(6..).filter(|_| {
            bytes
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"basic"))
                && bytes.get(5) == Some(&b' ')
        }) else {
            return false;
        };
        if values.next().is_some() || encoded.len() > MAX_BASIC_CREDENTIAL_BYTES * 2 {
            return false;
        }
        let Ok(decoded) = STANDARD.decode(encoded) else {
            return false;
        };
        let decoded = Zeroizing::new(decoded);
        if decoded.len() > MAX_BASIC_CREDENTIAL_BYTES {
            return false;
        }
        let Some(separator) = decoded.iter().position(|byte| *byte == b':') else {
            return false;
        };
        let Ok(username) = std::str::from_utf8(&decoded[..separator]) else {
            return false;
        };
        let Ok(password) = std::str::from_utf8(&decoded[separator + 1..]) else {
            return false;
        };
        let hash = self
            .users
            .iter()
            .find(|(candidate, _)| candidate == username)
            .map(|(_, hash)| hash.clone());
        let known_user = hash.is_some();
        let hash = hash.unwrap_or_else(|| self.dummy_hash.clone());
        let password = Zeroizing::new(password.to_owned());
        let semaphore = bcrypt_semaphore();
        let Ok(permit) = semaphore.try_acquire_owned() else {
            return false;
        };
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            bcrypt::verify(password.as_str(), &hash)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|verified| known_user && verified)
    }
}

fn bcrypt_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_BCRYPT_VERIFICATIONS,
        ))
    }))
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
        } = policy
        else {
            return Err(AccessPreflightError);
        };
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
        std::io::Read::by_ref(&mut file)
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
    directory_policy: StaticDirectoryPolicy,
    fallback: Option<Box<[OsString]>>,
    mapping: HttpStaticPathMapping,
    mount_path: String,
    directory_redirects: bool,
    try_files: Box<[StaticTryFilePlan]>,
    mime: HashMap<String, HeaderValue>,
    default_type: Option<HeaderValue>,
    headers: Box<[(HeaderName, HeaderValue, bool)]>,
    error_responses: HashMap<u16, StaticErrorResponsePlan>,
}

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "directory listing, timestamp, size, and nginx index behavior are independent policies"
)]
struct StaticDirectoryPolicy {
    indexes: Box<[OsString]>,
    autoindex: bool,
    exact_size: bool,
    local_time: bool,
    internal_index_redirects: bool,
}

impl StaticDirectoryPolicy {
    fn disabled() -> Self {
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
enum StaticTryFilePlan {
    RequestPath,
    RequestPathDirectory,
    Relative(Box<[OsString]>),
    Status(u16),
}

#[derive(Clone, Debug)]
enum StaticErrorResponsePlan {
    File(Box<[OsString]>),
    InternalRedirect(String),
}

impl StaticFilesPlan {
    #[expect(
        clippy::too_many_lines,
        reason = "one secure preflight compiles and pins the complete static action"
    )]
    pub(crate) fn open(
        mount_path: &str,
        action: &HttpRouteAction,
    ) -> Result<Self, StaticPreflightError> {
        let HttpRouteAction::StaticFiles {
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
                if let Some(path) = &response.internal_redirect {
                    return Ok((
                        status,
                        StaticErrorResponsePlan::InternalRedirect(path.clone()),
                    ));
                }
                path_components(&response.file)
                    .map(|components| {
                        (
                            status,
                            StaticErrorResponsePlan::File(components.into_boxed_slice()),
                        )
                    })
                    .map_err(|()| StaticPreflightError)
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(Self {
            root: Arc::new(
                open_pinned_directory(root_directory).map_err(|_| StaticPreflightError)?,
            ),
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

    pub(crate) async fn serve(&self, request_path: &str) -> Result<StaticTarget, StaticServeError> {
        let components = self.request_components(request_path)?;
        let root = Arc::clone(&self.root);
        let directory_policy = self.directory_policy.clone();
        let fallback = self.fallback.clone();
        let try_files = self.try_files.clone();
        let directory_redirects = self.directory_redirects;
        let request_has_trailing_slash = request_path.ends_with('/');
        let request_path = request_path.to_owned();
        tokio::task::spawn_blocking(move || {
            if try_files.is_empty() {
                return match read_static_target(
                    &root,
                    &components,
                    &directory_policy,
                    false,
                    directory_redirects && !request_has_trailing_slash,
                    &request_path,
                ) {
                    Err(StaticServeError::NotFound) if fallback.is_some() => read_static_target(
                        &root,
                        fallback.as_deref().expect("checked fallback"),
                        &StaticDirectoryPolicy::disabled(),
                        false,
                        false,
                        &request_path,
                    ),
                    result => result,
                };
            }
            for candidate in &try_files {
                let result = match candidate {
                    StaticTryFilePlan::RequestPath => read_static_target(
                        &root,
                        &components,
                        &directory_policy,
                        false,
                        directory_redirects && !request_has_trailing_slash,
                        &request_path,
                    ),
                    StaticTryFilePlan::RequestPathDirectory => read_static_target(
                        &root,
                        &components,
                        &directory_policy,
                        true,
                        directory_redirects && !request_has_trailing_slash,
                        &request_path,
                    ),
                    StaticTryFilePlan::Relative(path) => read_static_target(
                        &root,
                        path,
                        &directory_policy,
                        false,
                        false,
                        &request_path,
                    ),
                    StaticTryFilePlan::Status(status) => return Ok(StaticTarget::Status(*status)),
                };
                match result {
                    Err(StaticServeError::NotFound) => {}
                    result => return result,
                }
            }
            Err(StaticServeError::NotFound)
        })
        .await
        .map_err(|_| StaticServeError::Unavailable)?
    }

    pub(crate) async fn error_document(&self, status: u16) -> Option<StaticErrorTarget> {
        let response = self.error_responses.get(&status)?.clone();
        let StaticErrorResponsePlan::File(components) = response else {
            let StaticErrorResponsePlan::InternalRedirect(path) = response else {
                unreachable!();
            };
            return Some(StaticErrorTarget::InternalRedirect(path));
        };
        let root = Arc::clone(&self.root);
        tokio::task::spawn_blocking(move || {
            read_static_target(
                &root,
                &components,
                &StaticDirectoryPolicy::disabled(),
                false,
                false,
                "",
            )
            .ok()
            .and_then(|target| match target {
                StaticTarget::File(file) => Some(StaticErrorTarget::File(file)),
                StaticTarget::Autoindex { .. }
                | StaticTarget::DirectoryRedirect { .. }
                | StaticTarget::InternalRedirect { .. }
                | StaticTarget::Status(_) => None,
            })
        })
        .await
        .ok()
        .flatten()
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

pub(crate) fn nginx_add_header_status(status: u16) -> bool {
    matches!(
        status,
        200 | 201 | 204 | 206 | 301 | 302 | 303 | 304 | 307 | 308
    )
}

#[derive(Debug)]
pub(crate) struct StaticFile {
    pub(crate) etag: HeaderValue,
    pub(crate) file: File,
    pub(crate) modified: std::time::SystemTime,
    pub(crate) name: OsString,
    pub(crate) size: u64,
}

#[derive(Debug)]
pub(crate) enum StaticTarget {
    File(StaticFile),
    Autoindex { body: Bytes },
    DirectoryRedirect { path: String },
    InternalRedirect { path: String },
    Status(u16),
}

#[derive(Debug)]
pub(crate) enum StaticErrorTarget {
    File(StaticFile),
    InternalRedirect(String),
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
    policy: &StaticDirectoryPolicy,
    require_directory: bool,
    redirect_directory: bool,
    request_path: &str,
) -> Result<StaticTarget, StaticServeError> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory =
        rustix::io::fcntl_dupfd_cloexec(root, 0).map_err(|_| StaticServeError::Unavailable)?;
    let Some((file_name, parents)) = components.split_last() else {
        return read_directory(&directory, policy, false, request_path);
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
        return read_directory(&descriptor, policy, redirect_directory, request_path);
    }
    if require_directory {
        return Err(StaticServeError::NotFound);
    }
    read_regular_file(descriptor, file_name).map(StaticTarget::File)
}

fn read_directory(
    directory: &OwnedFd,
    policy: &StaticDirectoryPolicy,
    redirect: bool,
    request_path: &str,
) -> Result<StaticTarget, StaticServeError> {
    if redirect {
        return Ok(StaticTarget::DirectoryRedirect {
            path: format!("{request_path}/"),
        });
    }
    for index in &policy.indexes {
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
            Ok(_file) if policy.internal_index_redirects => {
                let Some(index) = index.to_str() else {
                    return Err(StaticServeError::Unsafe);
                };
                return Ok(StaticTarget::InternalRedirect {
                    path: format!("{request_path}{index}"),
                });
            }
            result => return result.map(StaticTarget::File),
        }
    }
    if policy.autoindex {
        render_autoindex(directory, policy.exact_size, policy.local_time)
            .map(|body| StaticTarget::Autoindex { body })
    } else {
        Err(StaticServeError::Unsafe)
    }
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
    let size = u64::try_from(before.st_size).map_err(|_| StaticServeError::TooLarge)?;
    if size > MAX_STATIC_FILE_BYTES {
        return Err(StaticServeError::TooLarge);
    }
    Ok(StaticFile {
        etag: HeaderValue::from_str(&format!(
            "\"{:x}-{:x}-{:x}-{:x}\"",
            before.st_dev, before.st_ino, before.st_size, before.st_mtime
        ))
        .expect("stat fields produce a valid ETag"),
        file: File::from(descriptor),
        modified: u64::try_from(before.st_mtime)
            .ok()
            .and_then(|seconds| {
                std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
            })
            .unwrap_or(std::time::UNIX_EPOCH),
        name: name.to_os_string(),
        size,
    })
}

struct AutoindexEntry {
    directory: bool,
    modified: i64,
    name: Vec<u8>,
    size: u64,
}

fn render_autoindex(
    directory: &OwnedFd,
    exact_size: bool,
    local_time: bool,
) -> Result<Bytes, StaticServeError> {
    let mut entries = Vec::new();
    let mut reader = Dir::read_from(directory).map_err(|_| StaticServeError::Unavailable)?;
    for entry in &mut reader {
        let entry = entry.map_err(|_| StaticServeError::Unavailable)?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if entries.len() >= MAX_AUTOINDEX_ENTRIES {
            return Err(StaticServeError::TooLarge);
        }
        let metadata =
            match rustix_fs::statat(directory, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(Errno::NOENT) => continue,
                Err(_) => return Err(StaticServeError::Unavailable),
            };
        let kind = FileType::from_raw_mode(metadata.st_mode);
        if !kind.is_file() && !kind.is_dir() {
            continue;
        }
        entries.push(AutoindexEntry {
            directory: kind.is_dir(),
            modified: metadata.st_mtime,
            name: name.to_vec(),
            size: u64::try_from(metadata.st_size).unwrap_or(0),
        });
    }
    entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));

    let mut output = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Index</title></head><body><h1>Index</h1><pre><a href=\"../\">../</a>\n",
    );
    for entry in entries {
        let href = percent_encode_path_segment(&entry.name);
        let label = html_escape(&String::from_utf8_lossy(&entry.name));
        let suffix = if entry.directory { "/" } else { "" };
        let modified = autoindex_time(entry.modified, local_time);
        let size = if entry.directory {
            "-".to_owned()
        } else if exact_size {
            entry.size.to_string()
        } else {
            human_size(entry.size)
        };
        writeln!(
            output,
            "<a href=\"{href}{suffix}\">{label}{suffix}</a>  {modified}  {size}"
        )
        .map_err(|_| StaticServeError::Unavailable)?;
        if output.len() > MAX_AUTOINDEX_BYTES {
            return Err(StaticServeError::TooLarge);
        }
    }
    output.push_str("</pre></body></html>\n");
    Ok(Bytes::from(output))
}

fn autoindex_time(timestamp: i64, local: bool) -> String {
    let Ok(mut value) = time::OffsetDateTime::from_unix_timestamp(timestamp) else {
        return "1970-01-01 00:00".into();
    };
    if local {
        if let Ok(offset) = time::UtcOffset::local_offset_at(value) {
            value = value.to_offset(offset);
        }
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute()
    )
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 && unit < UNITS.len() - 1 {
        value = value.saturating_add(1023) / 1024;
        unit += 1;
    }
    format!("{value}{}", UNITS[unit])
}

fn percent_encode_path_segment(value: &[u8]) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn html_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
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

fn read_secret_file(
    path: &Path,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>, AccessPreflightError> {
    let descriptor = rustix_fs::open(
        path,
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
    if size > max_bytes {
        return Err(AccessPreflightError);
    }
    let mut file = File::from(descriptor);
    let mut bytes = Zeroizing::new(Vec::with_capacity(size));
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(max_bytes + 1).map_err(|_| AccessPreflightError)?)
        .read_to_end(&mut bytes)
        .map_err(|_| AccessPreflightError)?;
    let after = rustix_fs::fstat(&file).map_err(|_| AccessPreflightError)?;
    if bytes.len() > max_bytes || bytes.len() != size || !same_file_snapshot(&before, &after) {
        return Err(AccessPreflightError);
    }
    Ok(bytes)
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

pub(crate) fn builtin_content_type(name: &OsStr) -> &'static str {
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

#[cfg(test)]
mod access_log_tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn access_log_rejects_a_symlinked_ancestor() {
        let directory = tempfile::tempdir().expect("access log fixture directory");
        let real = directory.path().join("real");
        std::fs::create_dir(&real).expect("real access log directory");
        let linked = directory.path().join("linked");
        symlink(&real, &linked).expect("symlinked access log directory");

        let policy = AccessLogPolicy::File {
            path: linked.join("access.jsonl"),
        };
        assert!(AccessLog::open("test", Some(&policy)).is_err());
        assert!(!real.join("access.jsonl").exists());
    }

    #[test]
    fn access_log_queue_saturation_never_blocks_the_caller() {
        assert_eq!(ACCESS_LOG_QUEUE_CAPACITY, 1_024);
        let (sender, _receiver) = mpsc::sync_channel(1);
        let access_log = AccessLog {
            sender: Some(sender),
            service: "test".into(),
            worker: Mutex::new(None),
        };
        let event = serde_json::json!({"status": 200});

        access_log.write(&event).expect("first queued event");
        let error = access_log.write(&event).expect_err("full queue rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }
}
