use std::{
    collections::{BTreeSet, HashMap, HashSet},
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
use http::{HeaderMap, HeaderName, HeaderValue, Method, header::AUTHORIZATION};
use openssl::{
    hash::{Hasher, MessageDigest},
    memcmp,
    sha::sha256,
};
use oxiroute_cache::{
    BaseKey, Cache, CacheConfig, CacheError, CacheKey, CacheResponse, CacheTimeline,
    CachedResponse, DiskCache, DiskCacheConfig, DiskCacheError, DiskFillGuard, FillGuard, FillJoin,
    Lookup, MonoTime, PreparedEntry, PurgeResult, RequestKeyInput, ResponseTiming, StoreOutcome,
};
use oxiroute_config::{
    AccessLogPolicy, HttpAccessPolicy, HttpCookieAttributePolicy, HttpCookiePathRewrite,
    HttpGzipMinimumVersion, HttpGzipPolicy, HttpLiteralHeader, HttpProxyPathRewrite,
    HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryTarget, HttpRetryTrigger, HttpRouteAction,
    HttpRoutePolicy, HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost,
};
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags},
    io::Errno,
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::upstream_peer::UpstreamPlan;

const MIN_ACCESS_TOKEN_BYTES: usize = 32;
const MAX_ACCESS_TOKEN_BYTES: usize = 512;
const MAX_ACCESS_TOKEN_FILE_BYTES: usize = MAX_ACCESS_TOKEN_BYTES + 2;
const MAX_HTPASSWD_FILE_BYTES: usize = 1024 * 1024;
const MAX_BASIC_CREDENTIAL_BYTES: usize = 2048;
const MAX_BASIC_USERNAME_BYTES: usize = 256;
const MAX_BASIC_HASH_BYTES: usize = 60;
const MAX_CONCURRENT_BASIC_VERIFICATIONS: usize = 4;
const MIN_BASIC_BCRYPT_COST: u32 = 4;
const MAX_BASIC_BCRYPT_COST: u32 = 12;
const APR1_PREFIX: &str = "$apr1$";
const MAX_APR1_SALT_BYTES: usize = 8;
const APR1_DIGEST_BYTES: usize = 22;
const APR1_ROUNDS: usize = 1_000;
const APR1_ALPHABET: &[u8; 64] =
    b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
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
    pub(crate) min_length_bytes: usize,
    pub(crate) min_http_version: HttpGzipMinimumVersion,
    pub(crate) disable_on_via: bool,
    pub(crate) vary: bool,
}

impl HttpGzipPlan {
    pub(crate) fn compile(policy: &HttpGzipPolicy) -> Self {
        Self {
            level: u32::from(policy.level),
            content_types: policy.content_types.clone().into_boxed_slice(),
            min_length_bytes: usize::try_from(policy.min_length_bytes)
                .expect("validated gzip length fits usize"),
            min_http_version: policy.min_http_version,
            disable_on_via: policy.disable_on_via,
            vary: policy.vary,
        }
    }
}

pub(crate) struct AccessLog {
    sender: Option<SyncSender<Vec<u8>>>,
    service: String,
    rtmp_metrics: Option<Arc<crate::logging::RtmpAccessLogMetrics>>,
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
        Self::open_with_rtmp_metrics(service, policy, None)
    }

    pub(crate) fn open_rtmp(
        service: &str,
        policy: Option<&AccessLogPolicy>,
    ) -> Result<Option<Self>, AccessPreflightError> {
        Self::open_with_rtmp_metrics(
            service,
            policy,
            Some(crate::logging::rtmp_access_log_metrics()),
        )
    }

    fn open_with_rtmp_metrics(
        service: &str,
        policy: Option<&AccessLogPolicy>,
        rtmp_metrics: Option<Arc<crate::logging::RtmpAccessLogMetrics>>,
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
        let worker_metrics = rtmp_metrics.clone();
        let worker_name = if rtmp_metrics.is_some() {
            "rtmp-access-log"
        } else {
            "http-access-log"
        };
        let worker = std::thread::Builder::new()
            .name(format!("{worker_name}-{service}"))
            .spawn(move || {
                let mut file = File::from(descriptor);
                while let Ok(line) = receiver.recv() {
                    if let Some(metrics) = &worker_metrics {
                        metrics.worker_received();
                    }
                    if file.write_all(&line).is_err() || file.write_all(b"\n").is_err() {
                        if let Some(metrics) = &worker_metrics {
                            metrics.worker_failed();
                            for _ in receiver.try_iter() {
                                metrics.worker_received();
                            }
                        }
                        break;
                    }
                    if let Some(metrics) = &worker_metrics {
                        metrics.worker_written();
                    }
                }
            })
            .map_err(|_| AccessPreflightError)?;
        Ok(Some(Self {
            sender: Some(sender),
            service: service.to_owned(),
            rtmp_metrics,
            worker: Mutex::new(Some(worker)),
        }))
    }

    pub(crate) fn write(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.enqueue(serde_json::to_vec(&crate::logging::redact_access_record(
            event,
        ))?)
    }

    pub(crate) fn write_rtmp(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.enqueue(serde_json::to_vec(
            &crate::logging::redact_rtmp_access_record(event),
        )?)
    }

    fn enqueue(&self, line: Vec<u8>) -> std::io::Result<()> {
        if let Some(metrics) = &self.rtmp_metrics {
            metrics.queue_event();
        }
        self.sender
            .as_ref()
            .expect("access log sender exists until final drop")
            .try_send(line)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    if let Some(metrics) = &self.rtmp_metrics {
                        metrics.queue_event_rejected(true);
                    }
                    std::io::Error::new(std::io::ErrorKind::WouldBlock, "access log queue is full")
                }
                TrySendError::Disconnected(_) => {
                    if let Some(metrics) = &self.rtmp_metrics {
                        metrics.queue_event_rejected(false);
                    }
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
    pub(crate) request_buffering: bool,
    pub(crate) response_buffering: bool,
    pub(crate) connect_timeout: std::time::Duration,
    pub(crate) read_timeout: std::time::Duration,
    pub(crate) write_timeout: std::time::Duration,
}

impl RoutePolicyPlan {
    pub(crate) fn compile(policy: HttpRoutePolicy) -> Self {
        Self {
            max_request_body_bytes: policy.max_request_body_bytes,
            request_buffering: policy.request_buffering,
            response_buffering: policy.response_buffering,
            connect_timeout: std::time::Duration::from_millis(policy.connect_timeout_ms),
            read_timeout: std::time::Duration::from_millis(policy.read_timeout_ms),
            write_timeout: std::time::Duration::from_millis(policy.write_timeout_ms),
        }
    }

    pub(crate) fn exceeds_body_limit(self, bytes: u64) -> bool {
        self.max_request_body_bytes
            .is_some_and(|limit| bytes > limit)
    }

    pub(crate) fn request_body_buffer_limit(self) -> Option<usize> {
        self.request_buffering
            .then_some(self.max_request_body_bytes)
            .flatten()
            .and_then(|limit| usize::try_from(limit).ok())
    }

    pub(crate) fn response_body_buffer_limit(self) -> Option<usize> {
        self.response_buffering
            .then_some(self.max_request_body_bytes)
            .flatten()
            .and_then(|limit| usize::try_from(limit).ok())
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
    pub(crate) upstream_path_rewrite: Option<HttpProxyPathRewrite>,
    pub(crate) request_headers: Box<[RequestHeaderMutationPlan]>,
    pub(crate) response_headers: Box<[ResponseHeaderMutationPlan]>,
    pub(crate) cookie_path_rewrites: Box<[HttpCookiePathRewrite]>,
    pub(crate) cookie_attributes: Box<[HttpCookieAttributePolicy]>,
    pub(crate) max_retries: u8,
    pub(crate) retry_triggers: Box<[HttpRetryTrigger]>,
    pub(crate) retry_response_statuses: Box<[u16]>,
    pub(crate) retry_target: HttpRetryTarget,
    pub(crate) retry_delay: Duration,
    pub(crate) final_redispatch: bool,
    pub(crate) cache: Option<Arc<HttpCachePlan>>,
    pub(crate) nginx_error_server: Option<HeaderValue>,
}

pub(crate) struct HttpCachePlan {
    pub(crate) cache: Arc<HttpCacheBackend>,
    pub(crate) timeline: CacheTimeline,
    pub(crate) methods: Box<[http::Method]>,
    pub(crate) revalidate: bool,
    pub(crate) surrogate_header: Option<HeaderName>,
    pub(crate) surrogate_limits: Option<(usize, usize)>,
    pub(crate) purge_access: Option<CachePurgeAccess>,
}

#[derive(Clone)]
pub(crate) struct CacheRequest {
    pub(crate) method: Method,
    pub(crate) scheme: &'static str,
    pub(crate) authority: String,
    pub(crate) path: String,
    pub(crate) query: Option<String>,
    pub(crate) headers: HeaderMap,
    pub(crate) request_started: MonoTime,
}

impl CacheRequest {
    pub(crate) fn input(&self) -> RequestKeyInput<'_> {
        RequestKeyInput {
            method: &self.method,
            scheme: self.scheme,
            authority: &self.authority,
            path: &self.path,
            query: self.query.as_deref(),
            headers: &self.headers,
        }
    }

    pub(crate) fn representation_input(&self) -> RequestKeyInput<'_> {
        RequestKeyInput {
            method: &Method::GET,
            scheme: self.scheme,
            authority: &self.authority,
            path: &self.path,
            query: self.query.as_deref(),
            headers: &self.headers,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheBackendError {
    #[error(transparent)]
    Memory(#[from] CacheError),
    #[error(transparent)]
    Disk(#[from] DiskCacheError),
    #[error("persistent cache I/O capacity is exhausted")]
    IoAtCapacity,
    #[error("persistent cache blocking task failed")]
    BlockingTask,
}

impl CacheBackendError {
    pub(crate) fn is_invalid_request(&self) -> bool {
        let error = match self {
            Self::Memory(error) | Self::Disk(DiskCacheError::Cache(error)) => error,
            Self::IoAtCapacity | Self::BlockingTask | Self::Disk(_) => return false,
        };
        matches!(
            error,
            CacheError::InvalidRequest(_) | CacheError::InvalidKey(_) | CacheError::InvalidTag
        )
    }
}

pub(crate) enum HttpCacheBackend {
    Memory(Arc<Cache>),
    Disk(Arc<DiskBackend>),
}

impl std::fmt::Debug for HttpCacheBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(_) => formatter.write_str("HttpCacheBackend::Memory"),
            Self::Disk(_) => formatter.write_str("HttpCacheBackend::Disk"),
        }
    }
}

pub(crate) struct DiskBackend {
    cache: Arc<DiskCache>,
    io: Arc<Semaphore>,
}

impl DiskBackend {
    pub(crate) fn new(cache: Arc<DiskCache>) -> Self {
        let permits = cache.config().memory.max_in_flight;
        Self {
            cache,
            io: Arc::new(Semaphore::new(permits)),
        }
    }

    pub(crate) fn disk_config(&self) -> &DiskCacheConfig {
        self.cache.config()
    }

    async fn run<T, F>(&self, operation: F) -> Result<T, CacheBackendError>
    where
        T: Send + 'static,
        F: FnOnce(&DiskCache) -> Result<T, DiskCacheError> + Send + 'static,
    {
        let permit = self
            .io
            .clone()
            .try_acquire_owned()
            .map_err(|_| CacheBackendError::IoAtCapacity)?;
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(&cache)
        })
        .await
        .map_err(|_| CacheBackendError::BlockingTask)?
        .map_err(Into::into)
    }

    async fn store(
        &self,
        guard: DiskFillGuard,
        entry: PreparedEntry,
    ) -> Result<StoreOutcome, CacheBackendError> {
        self.run(move |_| guard.store(entry)).await
    }
}

pub(crate) enum CacheFillJoin {
    Leader(CacheFill),
    Follower(oxiroute_cache::FillWaiter),
    AtCapacity,
}

pub(crate) enum CacheFill {
    Memory(FillGuard),
    Disk {
        guard: DiskFillGuard,
        backend: Arc<DiskBackend>,
    },
}

impl CacheFill {
    pub(crate) async fn store(
        self,
        entry: PreparedEntry,
    ) -> Result<StoreOutcome, CacheBackendError> {
        match self {
            Self::Memory(guard) => guard.store(entry).map_err(Into::into),
            Self::Disk { guard, backend } => backend.store(guard, entry).await,
        }
    }

    pub(crate) fn complete_without_store(self) -> bool {
        match self {
            Self::Memory(guard) => guard.complete_without_store(),
            Self::Disk { guard, .. } => guard.complete_without_store(),
        }
    }
}

impl HttpCacheBackend {
    pub(crate) fn config(&self) -> &CacheConfig {
        match self {
            Self::Memory(cache) => cache.config(),
            Self::Disk(backend) => &backend.cache.config().memory,
        }
    }

    pub(crate) fn now(&self) -> MonoTime {
        match self {
            Self::Memory(cache) => cache.now(),
            Self::Disk(backend) => backend.cache.now(),
        }
    }

    pub(crate) fn base(&self, request: &CacheRequest) -> Result<BaseKey, CacheBackendError> {
        BaseKey::new(request.representation_input(), self.config().max_key_bytes)
            .map_err(|error| CacheBackendError::Memory(CacheError::InvalidKey(error)))
    }

    pub(crate) fn prepare_with_timeline(
        &self,
        request: RequestKeyInput<'_>,
        response: CacheResponse<'_>,
        timeline: &CacheTimeline,
    ) -> Result<PreparedEntry, CacheBackendError> {
        match self {
            Self::Memory(cache) => cache
                .prepare_with_timeline(request, response, timeline)
                .map_err(Into::into),
            Self::Disk(backend) => backend
                .cache
                .prepare_with_timeline(request, response, timeline)
                .map_err(Into::into),
        }
    }

    pub(crate) fn prepare_not_modified_with_timeline(
        &self,
        request: RequestKeyInput<'_>,
        key: &CacheKey,
        not_modified: &HeaderMap,
        timing: ResponseTiming,
        timeline: &CacheTimeline,
    ) -> Result<PreparedEntry, CacheBackendError> {
        match self {
            Self::Memory(cache) => cache
                .prepare_not_modified_with_timeline(request, key, not_modified, timing, timeline)
                .map_err(Into::into),
            Self::Disk(backend) => backend
                .cache
                .prepare_not_modified_with_timeline(request, key, not_modified, timing, timeline)
                .map_err(Into::into),
        }
    }

    pub(crate) async fn lookup(&self, request: &CacheRequest) -> Result<Lookup, CacheBackendError> {
        match self {
            Self::Memory(cache) => cache.lookup(request.input()).map_err(Into::into),
            Self::Disk(backend) => {
                let request = request.clone();
                backend
                    .run(move |cache| cache.lookup(request.input()))
                    .await
            }
        }
    }

    pub(crate) async fn stale_if_error(
        &self,
        key: &CacheKey,
    ) -> Result<Option<CachedResponse>, CacheBackendError> {
        match self {
            Self::Memory(cache) => Ok(cache.stale_if_error(key)),
            Self::Disk(backend) => {
                let key = key.clone();
                backend.run(move |cache| cache.stale_if_error(&key)).await
            }
        }
    }

    pub(crate) async fn begin_fill(
        &self,
        base: BaseKey,
    ) -> Result<CacheFillJoin, CacheBackendError> {
        match self {
            Self::Memory(cache) => match cache.begin_fill(base).map_err(CacheBackendError::from)? {
                FillJoin::Leader(guard) => Ok(CacheFillJoin::Leader(CacheFill::Memory(guard))),
                FillJoin::Follower(waiter) => Ok(CacheFillJoin::Follower(waiter)),
                FillJoin::AtCapacity => Ok(CacheFillJoin::AtCapacity),
            },
            Self::Disk(backend) => {
                let backend = Arc::clone(backend);
                match backend.run(move |cache| cache.begin_fill(base)).await? {
                    oxiroute_cache::DiskFillJoin::Leader(guard) => {
                        Ok(CacheFillJoin::Leader(CacheFill::Disk { guard, backend }))
                    }
                    oxiroute_cache::DiskFillJoin::Follower(waiter) => {
                        Ok(CacheFillJoin::Follower(waiter))
                    }
                    oxiroute_cache::DiskFillJoin::AtCapacity => Ok(CacheFillJoin::AtCapacity),
                }
            }
        }
    }

    pub(crate) async fn purge_base(
        &self,
        base: &BaseKey,
    ) -> Result<PurgeResult, CacheBackendError> {
        match self {
            Self::Memory(cache) => Ok(cache.purge_base(base)),
            Self::Disk(backend) => {
                let base = base.clone();
                backend.run(move |cache| cache.purge_base(&base)).await
            }
        }
    }

    pub(crate) async fn purge_tag(&self, tag: &[u8]) -> Result<PurgeResult, CacheBackendError> {
        match self {
            Self::Memory(cache) => cache.purge_tag(tag).map_err(Into::into),
            Self::Disk(backend) => {
                let tag = tag.to_vec();
                backend.run(move |cache| cache.purge_tag(&tag)).await
            }
        }
    }
}

impl std::fmt::Debug for HttpCachePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpCachePlan")
            .field("methods", &self.methods)
            .field("revalidate", &self.revalidate)
            .field("surrogate_header", &self.surrogate_header)
            .field("surrogate_limits", &self.surrogate_limits)
            .field("purge_enabled", &self.purge_access.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpCachePlan {
    pub(crate) fn allows_method(&self, method: &http::Method) -> bool {
        self.methods.iter().any(|allowed| allowed == method)
    }

    pub(crate) fn cache_tags_within_limits(&self, tags: &[Bytes]) -> bool {
        self.surrogate_limits
            .is_none_or(|(max_tags, max_tag_bytes)| {
                tags.len() <= max_tags && tags.iter().all(|tag| tag.len() <= max_tag_bytes)
            })
    }
}

pub(crate) struct CachePurgeAccess {
    access: BearerTokenAccess,
}

impl CachePurgeAccess {
    pub(crate) fn load(path: &Path) -> Result<Self, AccessPreflightError> {
        let policy = HttpAccessPolicy::BearerTokenFile {
            token_file_path: path.to_owned(),
            header_name: "authorization".to_owned(),
            realm: None,
        };
        Ok(Self {
            access: BearerTokenAccess::load(&policy)?,
        })
    }

    pub(crate) fn authorizes(&self, headers: &HeaderMap) -> bool {
        self.access.authorizes(headers)
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        self.access.challenge()
    }
}

impl ProxyPolicyPlan {
    #[cfg(test)]
    pub(crate) fn compile(policy: &HttpProxyPolicy) -> Self {
        Self::compile_with_cache(policy, None)
    }

    pub(crate) fn compile_with_cache(
        policy: &HttpProxyPolicy,
        cache: Option<Arc<HttpCachePlan>>,
    ) -> Self {
        let nginx_content_type_marker = |mutation: &HttpResponseHeaderMutation| {
            matches!(
                mutation,
                HttpResponseHeaderMutation::Set {
                    name,
                    value,
                    always: true,
                } if name.eq_ignore_ascii_case("content-type")
                    && value.eq_ignore_ascii_case("text/html")
            )
        };
        let nginx_error_server = policy
            .response_headers
            .iter()
            .any(nginx_content_type_marker)
            .then(|| {
                policy.response_headers.iter().rev().find_map(|mutation| {
                    let HttpResponseHeaderMutation::Set {
                        name,
                        value,
                        always: true,
                    } = mutation
                    else {
                        return None;
                    };
                    name.eq_ignore_ascii_case("server").then(|| {
                        HeaderValue::from_str(value).expect("validated nginx server marker")
                    })
                })
            })
            .flatten();
        Self {
            upstream_host: policy.upstream_host.clone(),
            upstream_path_rewrite: policy.upstream_path_rewrite.clone(),
            request_headers: policy
                .request_headers
                .iter()
                .map(RequestHeaderMutationPlan::compile)
                .collect(),
            response_headers: policy
                .response_headers
                .iter()
                .filter(|mutation| {
                    nginx_error_server.is_none() || !nginx_content_type_marker(mutation)
                })
                .map(ResponseHeaderMutationPlan::compile)
                .collect(),
            cookie_path_rewrites: policy
                .response_cookie_path_rewrites
                .clone()
                .into_boxed_slice(),
            cookie_attributes: policy.response_cookie_attributes.clone().into_boxed_slice(),
            max_retries: policy.retry.max_retries,
            retry_triggers: policy.retry.triggers.clone().into_boxed_slice(),
            retry_response_statuses: policy.retry.response_statuses.clone().into_boxed_slice(),
            retry_target: policy.retry.target,
            retry_delay: Duration::from_millis(policy.retry.delay_ms),
            final_redispatch: policy.retry.final_redispatch,
            cache,
            nginx_error_server,
        }
    }

    pub(crate) fn retries_on(&self, trigger: HttpRetryTrigger) -> bool {
        self.retry_triggers.contains(&trigger)
    }

    pub(crate) fn retries_on_status(&self, status: u16) -> bool {
        self.retry_response_statuses.contains(&status)
    }

    pub(crate) fn target_for_retry(&self, attempts: usize) -> HttpRetryTarget {
        if self.final_redispatch && attempts == usize::from(self.max_retries) {
            HttpRetryTarget::NextServer
        } else {
            self.retry_target
        }
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
    dummy_hashes: Box<[BasicPasswordHash]>,
    username_case_sensitive: bool,
    users: Box<[BasicUser]>,
}

type BasicUser = ([u8; 32], Arc<str>, BasicPasswordHash);

#[derive(Clone)]
enum BasicPasswordHash {
    Bcrypt { hash: String, cost: u32 },
    Apr1(Apr1Hash),
}

#[derive(Clone)]
struct Apr1Hash {
    salt: Box<[u8]>,
    digest: [u8; APR1_DIGEST_BYTES],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BasicHashScheme {
    Bcrypt(u32),
    Apr1,
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
    pub(crate) fn load(path: &Path, realm: &str) -> Result<Self, AccessPreflightError> {
        Self::load_with_username_case(path, realm, true)
    }

    pub(crate) fn load_with_username_case(
        path: &Path,
        realm: &str,
        username_case_sensitive: bool,
    ) -> Result<Self, AccessPreflightError> {
        let bytes = read_secret_file(path, MAX_HTPASSWD_FILE_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| AccessPreflightError)?;
        let mut users = Vec::new();
        let mut username_digests = HashSet::new();
        let mut schemes = BTreeSet::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (username, hash) = line.split_once(':').ok_or(AccessPreflightError)?;
            if username.is_empty()
                || username.len() > MAX_BASIC_USERNAME_BYTES
                || username.bytes().any(|byte| byte.is_ascii_control())
                || hash.len() > MAX_BASIC_HASH_BYTES
            {
                return Err(AccessPreflightError);
            }
            let username_digest = sha256(username.as_bytes());
            if !username_digests.insert(username_digest) {
                return Err(AccessPreflightError);
            }
            let (parsed_hash, scheme) = BasicPasswordHash::parse(hash)?;
            schemes.insert(scheme);
            users.push((username_digest, Arc::<str>::from(username), parsed_hash));
        }
        if users.is_empty() {
            return Err(AccessPreflightError);
        }
        let dummy_hashes = schemes
            .into_iter()
            .map(BasicPasswordHash::dummy)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        Ok(Self {
            challenge: HeaderValue::from_str(&format!(
                "Basic realm=\"{realm}\", charset=\"UTF-8\""
            ))
            .expect("validated Basic realm"),
            dummy_hashes,
            username_case_sensitive,
            users: users.into_boxed_slice(),
        })
    }

    async fn authorizes(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        self.authenticate(value.as_bytes()).await.is_some()
    }

    pub(crate) async fn authenticate(&self, bytes: &[u8]) -> Option<Arc<str>> {
        let encoded = bytes.get(6..).filter(|_| {
            bytes
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"basic"))
                && bytes.get(5) == Some(&b' ')
        })?;
        if encoded.len() > MAX_BASIC_CREDENTIAL_BYTES * 2 {
            return None;
        }
        let Ok(decoded) = STANDARD.decode(encoded) else {
            return None;
        };
        let decoded = Zeroizing::new(decoded);
        if decoded.len() > MAX_BASIC_CREDENTIAL_BYTES {
            return None;
        }
        let separator = decoded.iter().position(|byte| *byte == b':')?;
        if separator > MAX_BASIC_USERNAME_BYTES {
            return None;
        }
        let Ok(username) = std::str::from_utf8(&decoded[..separator]) else {
            return None;
        };
        let Ok(password) = std::str::from_utf8(&decoded[separator + 1..]) else {
            return None;
        };
        let username_digest = basic_client_username_digest(username, self.username_case_sensitive);
        let mut selected_index = self.users.len();
        // Complete every comparison before selecting a hash so entry position does not alter scan work.
        for (index, (candidate_digest, _, _)) in self.users.iter().enumerate() {
            let matched = usize::from(memcmp::eq(&username_digest, candidate_digest));
            let mask = 0usize.wrapping_sub(matched);
            selected_index = (selected_index & !mask) | (index & mask);
        }
        let known_user = selected_index != self.users.len();
        let selected_hash = known_user.then(|| self.users[selected_index].2.clone());
        let hashes = self
            .dummy_hashes
            .iter()
            .map(|dummy| {
                selected_hash
                    .as_ref()
                    .filter(|hash| hash.scheme() == dummy.scheme())
                    .unwrap_or(dummy)
                    .clone()
            })
            .collect::<Box<[_]>>();
        let password = Zeroizing::new(password.to_owned());
        let semaphore = basic_auth_semaphore();
        let Ok(permit) = semaphore.try_acquire_owned() else {
            return None;
        };
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hashes.iter().any(|hash| hash.verify(password.as_bytes()))
        })
        .await
        .ok()
        .is_some_and(|verified| known_user && verified);
        verified.then(|| Arc::clone(&self.users[selected_index].1))
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        &self.challenge
    }
}

fn basic_client_username_digest(username: &str, case_sensitive: bool) -> [u8; 32] {
    if case_sensitive {
        sha256(username.as_bytes())
    } else {
        sha256(username.to_ascii_lowercase().as_bytes())
    }
}

impl BasicPasswordHash {
    fn parse(hash: &str) -> Result<(Self, BasicHashScheme), AccessPreflightError> {
        if matches!(hash.get(..4), Some("$2y$" | "$2b$" | "$2a$")) {
            if hash.len() != MAX_BASIC_HASH_BYTES {
                return Err(AccessPreflightError);
            }
            let parts = hash
                .parse::<bcrypt::HashParts>()
                .map_err(|_| AccessPreflightError)?;
            let cost = parts.get_cost();
            if !(MIN_BASIC_BCRYPT_COST..=MAX_BASIC_BCRYPT_COST).contains(&cost) {
                return Err(AccessPreflightError);
            }
            return Ok((
                Self::Bcrypt {
                    hash: hash.to_owned(),
                    cost,
                },
                BasicHashScheme::Bcrypt(cost),
            ));
        }

        let apr1 = Apr1Hash::parse(hash)?;
        Ok((Self::Apr1(apr1), BasicHashScheme::Apr1))
    }

    fn dummy(scheme: BasicHashScheme) -> Result<Self, AccessPreflightError> {
        match scheme {
            BasicHashScheme::Bcrypt(cost) => {
                let hash = bcrypt::hash_with_salt(
                    b"oxiroute-unknown-basic-user",
                    cost,
                    *b"OxiRouteDummy123",
                )
                .map_err(|_| AccessPreflightError)?
                .to_string();
                Ok(Self::Bcrypt { hash, cost })
            }
            BasicHashScheme::Apr1 => Ok(Self::Apr1(Apr1Hash {
                salt: Box::from(&b"OxiRoute"[..]),
                digest: apr1_digest(b"oxiroute-unknown-basic-user", b"OxiRoute")
                    .map_err(|_| AccessPreflightError)?,
            })),
        }
    }

    fn scheme(&self) -> BasicHashScheme {
        match self {
            Self::Bcrypt { cost, .. } => BasicHashScheme::Bcrypt(*cost),
            Self::Apr1(_) => BasicHashScheme::Apr1,
        }
    }

    fn verify(&self, password: &[u8]) -> bool {
        match self {
            Self::Bcrypt { hash, .. } => bcrypt::verify(password, hash).unwrap_or(false),
            Self::Apr1(hash) => apr1_digest(password, &hash.salt)
                .is_ok_and(|digest| memcmp::eq(&digest, &hash.digest)),
        }
    }
}

impl Apr1Hash {
    fn parse(hash: &str) -> Result<Self, AccessPreflightError> {
        let value = hash.strip_prefix(APR1_PREFIX).ok_or(AccessPreflightError)?;
        let (salt, digest) = value.split_once('$').ok_or(AccessPreflightError)?;
        if salt.is_empty()
            || salt.len() > MAX_APR1_SALT_BYTES
            || !salt.bytes().all(is_apr1_character)
            || digest.len() != APR1_DIGEST_BYTES
            || !digest.bytes().all(is_apr1_character)
            || !matches!(digest.as_bytes().last(), Some(b'.' | b'/' | b'0' | b'1'))
        {
            return Err(AccessPreflightError);
        }
        let mut parsed_digest = [0; APR1_DIGEST_BYTES];
        parsed_digest.copy_from_slice(digest.as_bytes());
        Ok(Self {
            salt: salt.as_bytes().into(),
            digest: parsed_digest,
        })
    }
}

fn is_apr1_character(byte: u8) -> bool {
    APR1_ALPHABET.contains(&byte)
}

fn apr1_digest(
    password: &[u8],
    salt: &[u8],
) -> Result<[u8; APR1_DIGEST_BYTES], openssl::error::ErrorStack> {
    let mut alternate = Hasher::new(MessageDigest::md5())?;
    alternate.update(password)?;
    alternate.update(salt)?;
    alternate.update(password)?;
    let alternate = alternate.finish()?;

    let mut initial = Hasher::new(MessageDigest::md5())?;
    initial.update(password)?;
    initial.update(APR1_PREFIX.as_bytes())?;
    initial.update(salt)?;
    let mut remaining = password.len();
    while remaining >= alternate.len() {
        initial.update(&alternate)?;
        remaining -= alternate.len();
    }
    initial.update(&alternate[..remaining])?;
    let mut length = password.len();
    while length != 0 {
        if length & 1 == 0 {
            initial.update(&password[..1])?;
        } else {
            initial.update(&[0])?;
        }
        length >>= 1;
    }
    let mut digest = initial.finish()?;

    for round in 0..APR1_ROUNDS {
        let mut hasher = Hasher::new(MessageDigest::md5())?;
        if round & 1 == 0 {
            hasher.update(&digest)?;
        } else {
            hasher.update(password)?;
        }
        if round % 3 != 0 {
            hasher.update(salt)?;
        }
        if round % 7 != 0 {
            hasher.update(password)?;
        }
        if round & 1 == 0 {
            hasher.update(password)?;
        } else {
            hasher.update(&digest)?;
        }
        digest = hasher.finish()?;
    }

    Ok(apr1_encode(
        digest
            .as_ref()
            .try_into()
            .expect("OpenSSL MD5 digest is 16 bytes"),
    ))
}

fn apr1_encode(digest: [u8; 16]) -> [u8; APR1_DIGEST_BYTES] {
    let mut encoded = [0; APR1_DIGEST_BYTES];
    let mut offset = 0;
    for (first, second, third) in [(0, 6, 12), (1, 7, 13), (2, 8, 14), (3, 9, 15), (4, 10, 5)] {
        offset = apr1_encode_group(
            &mut encoded,
            offset,
            digest[first],
            digest[second],
            digest[third],
            4,
        );
    }
    apr1_encode_group(&mut encoded, offset, 0, 0, digest[11], 2);
    encoded
}

fn apr1_encode_group(
    output: &mut [u8; APR1_DIGEST_BYTES],
    mut offset: usize,
    first: u8,
    second: u8,
    third: u8,
    count: usize,
) -> usize {
    let mut value = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);
    for _ in 0..count {
        output[offset] = APR1_ALPHABET[(value & 0x3f) as usize];
        offset += 1;
        value >>= 6;
    }
    offset
}

fn basic_auth_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_BASIC_VERIFICATIONS,
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
    etag: bool,
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
                StaticTarget::File(file) => Some(StaticErrorTarget::File { file, headers }),
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

    pub(crate) fn etag_enabled(&self) -> bool {
        self.etag
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
    File {
        file: StaticFile,
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
        etag: nginx_static_etag(before.st_mtime, size),
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

fn nginx_static_etag(modified: i64, size: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{modified:x}-{size:x}\""))
        .expect("stat fields produce a valid ETag")
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
    if local && let Ok(offset) = time::UtcOffset::local_offset_at(value) {
        value = value.to_offset(offset);
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
        || !matches!(before.st_mode & 0o7777, 0o400 | 0o600 | 0o440 | 0o640)
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
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    #[tokio::test]
    async fn mixed_htpasswd_schemes_authenticate_each_user() {
        let directory = tempfile::tempdir().expect("htpasswd directory");
        let path = directory.path().join("mixed.htpasswd");
        let apr1 = String::from_utf8(apr1_digest(b"alpha-pass", b"salt").unwrap().to_vec())
            .expect("APR1 digest");
        let bcrypt = bcrypt::hash_with_salt(b"bravo-pass", 4, *b"OxiRouteTest1234")
            .unwrap()
            .to_string();
        std::fs::write(&path, format!("alpha:$apr1$salt${apr1}\nbravo:{bcrypt}\n"))
            .expect("htpasswd file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("htpasswd mode");
        let access = BasicHtpasswdAccess::load(&path, "private").expect("mixed htpasswd");

        for (credentials, expected) in [
            ("alpha:alpha-pass", true),
            ("bravo:bravo-pass", true),
            ("alpha:wrong", false),
            ("missing:alpha-pass", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!(
                    "Basic {}",
                    STANDARD.encode(credentials.as_bytes())
                ))
                .unwrap(),
            );
            assert_eq!(access.authorizes(&headers).await, expected, "{credentials}");
        }

        let insensitive = BasicHtpasswdAccess::load_with_username_case(&path, "private", false)
            .expect("case-insensitive htpasswd");
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(b"ALPHA:alpha-pass")))
                .unwrap(),
        );
        assert!(insensitive.authorizes(&headers).await);

        let uppercase_path = directory.path().join("uppercase.htpasswd");
        std::fs::write(&uppercase_path, format!("ALPHA:$apr1$salt${apr1}\n"))
            .expect("uppercase htpasswd file");
        std::fs::set_permissions(&uppercase_path, std::fs::Permissions::from_mode(0o600))
            .expect("uppercase htpasswd mode");
        let uppercase =
            BasicHtpasswdAccess::load_with_username_case(&uppercase_path, "private", false)
                .expect("uppercase htpasswd");
        assert!(!uppercase.authorizes(&headers).await);
    }

    #[test]
    fn static_etags_use_nginx_mtime_and_size_format() {
        assert_eq!(nginx_static_etag(0x1234, 0xabcd), "\"1234-abcd\"");
    }

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
            rtmp_metrics: None,
            worker: Mutex::new(None),
        };
        let event = serde_json::json!({"status": 200});

        access_log.write(&event).expect("first queued event");
        let error = access_log.write(&event).expect_err("full queue rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn rtmp_access_log_queue_saturation_counts_nonblocking_drops() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let metrics = Arc::new(crate::logging::RtmpAccessLogMetrics::default());
        let access_log = AccessLog {
            sender: Some(sender),
            service: "test".into(),
            rtmp_metrics: Some(Arc::clone(&metrics)),
            worker: Mutex::new(None),
        };
        let event = serde_json::json!({"event": "connect"});

        access_log.write_rtmp(&event).expect("first queued event");
        let error = access_log
            .write_rtmp(&event)
            .expect_err("full RTMP queue rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.enqueued, 1);
        assert_eq!(snapshot.dropped, 1);
        assert_eq!(snapshot.queue_saturated, 1);
    }

    #[test]
    fn rtmp_access_log_drop_flushes_the_bounded_redacted_record() {
        let directory = tempfile::tempdir().expect("RTMP access log directory");
        let path = directory.path().join("rtmp-access.jsonl");
        let access_log =
            AccessLog::open_rtmp("live", Some(&AccessLogPolicy::File { path: path.clone() }))
                .expect("RTMP access log preflight")
                .expect("RTMP file sink");
        access_log
            .write_rtmp(&serde_json::json!({
                "timestampUnixMs": 1,
                "event": "publish",
                "result": "accepted",
                "listener": "live-listener",
                "service": "live",
                "application": "camera",
                "stream": "feed",
                "sessionId": "session-1",
                "role": "publisher",
                "bytesReceived": 2,
                "bytesSent": 3,
                "messagesReceived": 4,
                "messagesSent": 5,
                "durationMs": 6,
                "failureCode": null,
                "query": "token=secret",
                "clientIp": "192.0.2.1",
            }))
            .expect("queue RTMP access event");
        drop(access_log);

        let contents = std::fs::read_to_string(path).expect("flushed RTMP access log");
        let record: serde_json::Value =
            serde_json::from_str(contents.trim()).expect("JSONL record");
        assert_eq!(record["event"], "publish");
        assert_eq!(record["messagesSent"], 5);
        assert!(record.get("query").is_none());
        assert!(record.get("clientIp").is_none());
        assert!(!contents.contains("secret"));
    }
}
