use std::{path::Path, sync::Arc};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use oxiroute_cache::{
    BaseKey, Cache, CacheConfig, CacheControl, CacheError, CacheKey, CacheResponse, CacheTimeline,
    CachedResponse, DiskCache, DiskCacheConfig, DiskCacheError, DiskFillGuard, FillGuard, FillJoin,
    FillOutcome, Lookup, MonoTime, PreparedEntry, PurgeResult, RequestKeyInput, ResponseTiming,
    StoreOutcome, Validators,
};
use tokio::sync::Semaphore;

use crate::{
    ListenerMetrics,
    monitoring::CacheEvent,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};

const MAX_COLLAPSED_FILL_WAITS: u8 = 2;

pub(crate) struct HttpCachePlan {
    pub(crate) cache: Arc<HttpCacheBackend>,
    pub(crate) timeline: CacheTimeline,
    pub(crate) methods: Box<[Method]>,
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

    fn only_if_cached(&self) -> bool {
        CacheControl::parse(&self.headers)
            .ok()
            .is_some_and(|control| control.only_if_cached)
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
    pub(crate) fn allows_method(&self, method: &Method) -> bool {
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
    token: SecureBearerToken,
    challenge: HeaderValue,
}

impl CachePurgeAccess {
    pub(crate) fn load(path: &Path) -> Result<Self, CachePurgeAccessError> {
        Ok(Self {
            token: SecureBearerToken::load(path).map_err(|_| CachePurgeAccessError)?,
            challenge: HeaderValue::from_static("Bearer"),
        })
    }

    pub(crate) fn authorizes(&self, headers: &HeaderMap) -> bool {
        matches!(
            single_header(headers, &http::header::AUTHORIZATION),
            HeaderCardinality::Single(value) if self.token.authorizes(value.as_bytes())
        )
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        &self.challenge
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("HTTP cache purge access policy failed secure preflight")]
pub(crate) struct CachePurgeAccessError;

#[derive(Clone)]
pub(crate) struct CacheRevalidation {
    pub(crate) key: CacheKey,
    pub(crate) response: CachedResponse,
    pub(crate) validators: Validators,
    pub(crate) stale_if_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "protocol adapters migrate transaction failure ownership in a later slice"
)]
pub(crate) enum CacheFailureClass {
    Upstream,
    Local,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "protocol adapters migrate transaction failure ownership in a later slice"
)]
pub(crate) enum StaleEligibility {
    Allowed,
    LocalFailure,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "body adapters migrate transaction admission in a later slice"
)]
pub(crate) enum CacheAdmission {
    Stored { evicted: usize },
    GenerationLost,
}

pub(crate) enum CacheStartFailure {
    Policy,
    Lookup(CacheBackendError),
    AtCapacity,
    #[allow(
        dead_code,
        reason = "the outcome is retained for transaction diagnostics and tests"
    )]
    Follower(FillOutcome),
}

pub(crate) enum CacheStart {
    Bypass(CacheStartFailure),
    Hit(CachedResponse),
    OnlyIfCached,
    MissLeader(CacheTransaction),
    RevalidationLeader(CacheTransaction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheTransactionState {
    Lookup,
    MissLeader,
    RevalidationLeader,
    Completed,
    #[allow(
        dead_code,
        reason = "protocol adapters migrate explicit cancellation in a later slice"
    )]
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheTraceEvent {
    LookupBypass,
    LookupHit,
    LookupMiss,
    LookupRevalidate,
    FillLeader,
    FillFollower,
    FillAtCapacity,
    FollowerStored,
    FollowerStopped(FillOutcome),
    OnlyIfCached,
    #[allow(
        dead_code,
        reason = "admission traces are observed by transaction tests"
    )]
    Admitted(usize),
    #[allow(
        dead_code,
        reason = "generation traces are observed by transaction tests"
    )]
    GenerationLost,
    #[allow(
        dead_code,
        reason = "completion traces are observed by transaction tests"
    )]
    Completed,
    #[allow(
        dead_code,
        reason = "cancellation traces are observed by transaction tests"
    )]
    Cancelled,
}

pub(crate) struct CacheTransaction {
    plan: Arc<HttpCachePlan>,
    request: CacheRequest,
    listener: ListenerMetrics,
    fill: Option<CacheFill>,
    revalidation: Option<CacheRevalidation>,
    waits: u8,
    state: CacheTransactionState,
    #[cfg(test)]
    trace: Vec<CacheTraceEvent>,
}

pub(crate) struct CacheLeaderParts {
    pub(crate) plan: Arc<HttpCachePlan>,
    pub(crate) request: CacheRequest,
    pub(crate) fill: CacheFill,
    pub(crate) revalidation: Option<CacheRevalidation>,
}

impl CacheTransaction {
    pub(crate) fn new(
        plan: Arc<HttpCachePlan>,
        request: CacheRequest,
        listener: ListenerMetrics,
    ) -> Self {
        Self {
            plan,
            request,
            listener,
            fill: None,
            revalidation: None,
            waits: 0,
            state: CacheTransactionState::Lookup,
            #[cfg(test)]
            trace: Vec::new(),
        }
    }

    pub(crate) async fn start(mut self) -> CacheStart {
        loop {
            let lookup = match self.plan.cache.lookup(&self.request).await {
                Ok(lookup) => lookup,
                Err(error) => return CacheStart::Bypass(CacheStartFailure::Lookup(error)),
            };
            match lookup {
                Lookup::Bypass { .. } => {
                    self.trace(CacheTraceEvent::LookupBypass);
                    return CacheStart::Bypass(CacheStartFailure::Policy);
                }
                Lookup::Hit { response, .. } => {
                    self.trace(CacheTraceEvent::LookupHit);
                    self.record(CacheEvent::Hit);
                    self.state = CacheTransactionState::Completed;
                    return CacheStart::Hit(response);
                }
                Lookup::Miss {
                    only_if_cached,
                    base,
                    ..
                } => {
                    self.trace(CacheTraceEvent::LookupMiss);
                    self.record(CacheEvent::Miss);
                    if self.request.only_if_cached() || only_if_cached {
                        self.trace(CacheTraceEvent::OnlyIfCached);
                        self.state = CacheTransactionState::Completed;
                        return CacheStart::OnlyIfCached;
                    }
                    match self.join_fill(base).await {
                        FillStep::Leader(fill) => {
                            self.fill = Some(fill);
                            self.state = CacheTransactionState::MissLeader;
                            return CacheStart::MissLeader(self);
                        }
                        FillStep::Retry => {}
                        FillStep::Bypass(failure) => return CacheStart::Bypass(failure),
                    }
                }
                Lookup::Revalidate {
                    response,
                    validators,
                    stale_if_error,
                    ..
                } => {
                    self.trace(CacheTraceEvent::LookupRevalidate);
                    self.record(CacheEvent::Miss);
                    if self.request.only_if_cached() {
                        self.trace(CacheTraceEvent::OnlyIfCached);
                        self.state = CacheTransactionState::Completed;
                        return CacheStart::OnlyIfCached;
                    }
                    let base = response.key.base().clone();
                    match self.join_fill(base).await {
                        FillStep::Leader(fill) => {
                            self.fill = Some(fill);
                            self.revalidation = Some(CacheRevalidation {
                                key: response.key.clone(),
                                response,
                                validators,
                                stale_if_error,
                            });
                            self.state = CacheTransactionState::RevalidationLeader;
                            return CacheStart::RevalidationLeader(self);
                        }
                        FillStep::Retry => {}
                        FillStep::Bypass(failure) => return CacheStart::Bypass(failure),
                    }
                }
            }
        }
    }

    async fn join_fill(&mut self, base: BaseKey) -> FillStep {
        match self.plan.cache.begin_fill(base).await {
            Ok(CacheFillJoin::Leader(fill)) => {
                self.trace(CacheTraceEvent::FillLeader);
                FillStep::Leader(fill)
            }
            Ok(CacheFillJoin::Follower(waiter)) => {
                self.trace(CacheTraceEvent::FillFollower);
                self.waits = self.waits.saturating_add(1);
                if self.waits > MAX_COLLAPSED_FILL_WAITS {
                    return FillStep::Bypass(CacheStartFailure::Follower(FillOutcome::Filling));
                }
                let outcome = waiter.wait().await;
                if outcome == FillOutcome::Stored {
                    self.trace(CacheTraceEvent::FollowerStored);
                    FillStep::Retry
                } else if follower_should_retry(self.waits, outcome) {
                    self.trace(CacheTraceEvent::FollowerStopped(outcome));
                    FillStep::Retry
                } else {
                    self.trace(CacheTraceEvent::FollowerStopped(outcome));
                    FillStep::Bypass(CacheStartFailure::Follower(outcome))
                }
            }
            Ok(CacheFillJoin::AtCapacity) | Err(CacheBackendError::IoAtCapacity) => {
                self.trace(CacheTraceEvent::FillAtCapacity);
                FillStep::Bypass(CacheStartFailure::AtCapacity)
            }
            Err(error) => FillStep::Bypass(CacheStartFailure::Lookup(error)),
        }
    }

    #[allow(
        dead_code,
        reason = "protocol adapters migrate validator ownership in a later slice"
    )]
    pub(crate) fn validators(&self) -> Option<&Validators> {
        self.revalidation
            .as_ref()
            .map(|revalidation| &revalidation.validators)
    }

    #[allow(
        dead_code,
        reason = "protocol adapters migrate failure classification in a later slice"
    )]
    pub(crate) fn stale_eligibility(&self, failure: CacheFailureClass) -> StaleEligibility {
        match (failure, self.revalidation.as_ref()) {
            (CacheFailureClass::Local, _) => StaleEligibility::LocalFailure,
            (CacheFailureClass::Upstream, Some(revalidation)) if revalidation.stale_if_error => {
                StaleEligibility::Allowed
            }
            (CacheFailureClass::Upstream, _) => StaleEligibility::Unavailable,
        }
    }

    #[allow(
        dead_code,
        reason = "protocol adapters migrate stale response ownership in a later slice"
    )]
    pub(crate) async fn stale_response(
        &mut self,
        failure: CacheFailureClass,
    ) -> Option<CachedResponse> {
        if self.stale_eligibility(failure) != StaleEligibility::Allowed {
            return None;
        }
        let key = self.revalidation.as_ref()?.key.clone();
        self.plan.cache.stale_if_error(&key).await.ok().flatten()
    }

    #[allow(
        dead_code,
        reason = "body adapters migrate transaction admission in a later slice"
    )]
    pub(crate) async fn admit(
        &mut self,
        entry: PreparedEntry,
    ) -> Result<CacheAdmission, CacheBackendError> {
        let Some(fill) = self.fill.take() else {
            return Ok(CacheAdmission::GenerationLost);
        };
        let admission = match fill.store(entry).await? {
            StoreOutcome::Stored { evicted } => {
                self.record(CacheEvent::Admission);
                for _ in 0..evicted {
                    self.record(CacheEvent::Eviction);
                }
                self.trace(CacheTraceEvent::Admitted(evicted));
                CacheAdmission::Stored { evicted }
            }
            StoreOutcome::GenerationLost => {
                self.trace(CacheTraceEvent::GenerationLost);
                CacheAdmission::GenerationLost
            }
        };
        self.state = CacheTransactionState::Completed;
        Ok(admission)
    }

    #[allow(
        dead_code,
        reason = "body adapters migrate transaction completion in a later slice"
    )]
    pub(crate) fn complete_without_store(&mut self) -> bool {
        let completed = self
            .fill
            .take()
            .is_some_and(CacheFill::complete_without_store);
        self.state = CacheTransactionState::Completed;
        self.trace(CacheTraceEvent::Completed);
        completed
    }

    #[allow(
        dead_code,
        reason = "protocol adapters migrate explicit cancellation in a later slice"
    )]
    pub(crate) fn cancel(&mut self) {
        self.fill.take();
        self.state = CacheTransactionState::Cancelled;
        self.trace(CacheTraceEvent::Cancelled);
    }

    pub(crate) fn into_leader_parts(mut self) -> CacheLeaderParts {
        CacheLeaderParts {
            plan: self.plan,
            request: self.request,
            fill: self.fill.take().expect("leader transaction owns a fill"),
            revalidation: self.revalidation,
        }
    }

    fn record(&self, event: CacheEvent) {
        if let Err(error) = self.listener.record_cache_event(event) {
            log::warn!("could not account for HTTP cache event: {error}");
        }
    }

    #[cfg(test)]
    fn trace(&mut self, event: CacheTraceEvent) {
        self.trace.push(event);
    }

    #[cfg(not(test))]
    #[allow(
        clippy::unused_self,
        reason = "transaction trace storage is enabled only for deterministic tests"
    )]
    fn trace(&mut self, _event: CacheTraceEvent) {}
}

enum FillStep {
    Leader(CacheFill),
    Retry,
    Bypass(CacheStartFailure),
}

const fn follower_should_retry(waits: u8, outcome: FillOutcome) -> bool {
    matches!(outcome, FillOutcome::Stored) || waits < MAX_COLLAPSED_FILL_WAITS
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, SystemTime},
    };

    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue, Method, StatusCode};
    use oxiroute_cache::{Cache, CacheConfig, CacheResponse, CacheTimeline, Clock, MonoTime};

    use super::*;
    use crate::RuntimeMetrics;

    #[derive(Default)]
    struct ManualClock(AtomicU64);

    impl ManualClock {
        fn set(&self, seconds: u64) {
            self.0.store(seconds, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> MonoTime {
            MonoTime::from_duration(Duration::from_secs(self.0.load(Ordering::SeqCst)))
        }
    }

    struct Fixture {
        clock: Arc<ManualClock>,
        metrics: RuntimeMetrics,
        listener: ListenerMetrics,
        plan: Arc<HttpCachePlan>,
    }

    impl Fixture {
        fn new(mut config: CacheConfig) -> Self {
            config.max_total_bytes = config.max_total_bytes.max(64 * 1024);
            let clock = Arc::new(ManualClock::default());
            let cache = Cache::with_clock(config, clock.clone()).expect("cache");
            let metrics = RuntimeMetrics::new();
            let listener = metrics
                .register_listener("cache-kernel", "http", "127.0.0.1:8080", None)
                .expect("listener");
            let plan = Arc::new(HttpCachePlan {
                cache: Arc::new(HttpCacheBackend::Memory(Arc::new(cache))),
                timeline: CacheTimeline::new(
                    true,
                    Duration::from_mins(1),
                    [],
                    Duration::from_mins(1),
                    Duration::from_mins(2),
                )
                .expect("timeline"),
                methods: vec![Method::GET, Method::HEAD].into_boxed_slice(),
                revalidate: true,
                surrogate_header: None,
                surrogate_limits: None,
                purge_access: None,
            });
            Self {
                clock,
                metrics,
                listener,
                plan,
            }
        }

        fn transaction(&self, path: &str, headers: HeaderMap) -> CacheTransaction {
            let request = CacheRequest {
                method: Method::GET,
                scheme: "https",
                authority: "example.test".to_owned(),
                path: path.to_owned(),
                query: None,
                headers,
                request_started: self.plan.cache.now(),
            };
            CacheTransaction::new(Arc::clone(&self.plan), request, self.listener.clone())
        }

        fn cache_metrics(&self) -> crate::CacheSnapshot {
            self.metrics
                .snapshot()
                .expect("snapshot")
                .listeners
                .into_iter()
                .find(|listener| listener.name == "cache-kernel")
                .and_then(|listener| listener.cache)
                .expect("cache metrics")
        }
    }

    fn config() -> CacheConfig {
        CacheConfig {
            max_entries: 8,
            max_total_bytes: 64 * 1024,
            max_object_bytes: 8 * 1024,
            max_header_bytes: 2 * 1024,
            max_header_fields: 32,
            max_body_bytes: 4 * 1024,
            max_key_bytes: 1024,
            max_vary_fields: 8,
            max_tags_per_entry: 4,
            max_tag_bytes: 32,
            max_in_flight: 8,
            max_followers_per_fill: 8,
            max_heuristic_freshness: Duration::from_hours(24),
        }
    }

    fn response_headers(cache_control: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
        headers.insert(http::header::ETAG, HeaderValue::from_static("\"kernel\""));
        headers
    }

    async fn admit_response(
        transaction: &mut CacheTransaction,
        cache_control: &'static str,
        body: &'static [u8],
    ) -> CacheAdmission {
        let headers = response_headers(cache_control);
        let timing = ResponseTiming {
            request_started: transaction.request.request_started,
            response_received: transaction.plan.cache.now(),
            response_received_wall: SystemTime::now(),
        };
        let entry = transaction
            .plan
            .cache
            .prepare_with_timeline(
                transaction.request.representation_input(),
                CacheResponse {
                    status: StatusCode::OK,
                    headers: &headers,
                    body: Bytes::from_static(body),
                    timing,
                    tags: &[],
                },
                &transaction.plan.timeline,
            )
            .expect("prepared response");
        transaction.admit(entry).await.expect("admission")
    }

    async fn seed(fixture: &Fixture, path: &str, cache_control: &'static str) -> CacheAdmission {
        let CacheStart::MissLeader(mut transaction) =
            fixture.transaction(path, HeaderMap::new()).start().await
        else {
            panic!("initial request must lead the fill");
        };
        admit_response(&mut transaction, cache_control, b"cached").await
    }

    #[tokio::test]
    async fn transaction_trace_classifies_bypass_hit_and_only_if_cached() {
        let fixture = Fixture::new(config());
        let mut bypass_headers = HeaderMap::new();
        bypass_headers.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
        assert!(matches!(
            fixture.transaction("/bypass", bypass_headers).start().await,
            CacheStart::Bypass(CacheStartFailure::Policy)
        ));

        assert_eq!(
            seed(&fixture, "/hit", "public, max-age=60").await,
            CacheAdmission::Stored { evicted: 0 }
        );
        assert!(matches!(
            fixture
                .transaction("/hit", HeaderMap::new())
                .start()
                .await,
            CacheStart::Hit(response) if response.body == Bytes::from_static(b"cached")
        ));

        let mut only_if_cached = HeaderMap::new();
        only_if_cached.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("only-if-cached"),
        );
        assert!(matches!(
            fixture
                .transaction("/uncached", only_if_cached)
                .start()
                .await,
            CacheStart::OnlyIfCached
        ));
    }

    #[tokio::test]
    async fn transaction_trace_records_miss_leader_follower_and_cancellation() {
        let fixture = Fixture::new(config());
        let CacheStart::MissLeader(mut leader) = fixture
            .transaction("/collapsed", HeaderMap::new())
            .start()
            .await
        else {
            panic!("first request must lead");
        };
        assert_eq!(
            leader.trace,
            [CacheTraceEvent::LookupMiss, CacheTraceEvent::FillLeader]
        );

        let follower = fixture.transaction("/collapsed", HeaderMap::new());
        let follower_task = tokio::spawn(follower.start());
        tokio::task::yield_now().await;
        leader.cancel();

        let CacheStart::MissLeader(mut replacement) = follower_task.await.expect("follower task")
        else {
            panic!("cancelled fill must release replacement leadership");
        };
        assert_eq!(
            replacement.trace,
            [
                CacheTraceEvent::LookupMiss,
                CacheTraceEvent::FillFollower,
                CacheTraceEvent::FollowerStopped(FillOutcome::Cancelled),
                CacheTraceEvent::LookupMiss,
                CacheTraceEvent::FillLeader,
            ]
        );
        replacement.complete_without_store();
        assert_eq!(replacement.state, CacheTransactionState::Completed);
    }

    #[tokio::test]
    async fn transaction_trace_bounds_followers_and_reports_fill_capacity() {
        let mut limits = config();
        limits.max_in_flight = 1;
        let fixture = Fixture::new(limits);
        let CacheStart::MissLeader(mut leader) = fixture
            .transaction("/leader", HeaderMap::new())
            .start()
            .await
        else {
            panic!("first request must lead");
        };
        assert!(matches!(
            fixture
                .transaction("/capacity", HeaderMap::new())
                .start()
                .await,
            CacheStart::Bypass(CacheStartFailure::AtCapacity)
        ));

        leader.cancel();
        assert_eq!(leader.state, CacheTransactionState::Cancelled);
        assert!(follower_should_retry(1, FillOutcome::Cancelled));
        assert!(!follower_should_retry(2, FillOutcome::Cancelled));
        assert!(follower_should_retry(2, FillOutcome::Stored));
    }

    #[tokio::test]
    async fn transaction_trace_exposes_revalidation_and_classifies_stale_failures() {
        let fixture = Fixture::new(config());
        seed(
            &fixture,
            "/revalidate",
            "public, max-age=1, stale-if-error=60",
        )
        .await;
        fixture.clock.set(2);

        let CacheStart::RevalidationLeader(mut transaction) = fixture
            .transaction("/revalidate", HeaderMap::new())
            .start()
            .await
        else {
            panic!("expired response must revalidate");
        };
        assert!(transaction.validators().is_some());
        assert_eq!(
            transaction.stale_eligibility(CacheFailureClass::Upstream),
            StaleEligibility::Allowed
        );
        assert_eq!(
            transaction.stale_eligibility(CacheFailureClass::Local),
            StaleEligibility::LocalFailure
        );
        assert!(
            transaction
                .stale_response(CacheFailureClass::Upstream)
                .await
                .is_some()
        );
        assert!(
            transaction
                .stale_response(CacheFailureClass::Local)
                .await
                .is_none()
        );
        transaction.complete_without_store();
    }

    #[tokio::test]
    async fn transaction_trace_reports_generation_loss_without_admission_metrics() {
        let fixture = Fixture::new(config());
        let CacheStart::MissLeader(mut transaction) = fixture
            .transaction("/generation", HeaderMap::new())
            .start()
            .await
        else {
            panic!("request must lead");
        };
        let base = transaction
            .plan
            .cache
            .base(&transaction.request)
            .expect("base key");
        transaction
            .plan
            .cache
            .purge_base(&base)
            .await
            .expect("purge");

        assert_eq!(
            admit_response(&mut transaction, "public, max-age=60", b"lost").await,
            CacheAdmission::GenerationLost
        );
        assert_eq!(
            transaction.trace.last(),
            Some(&CacheTraceEvent::GenerationLost)
        );
        assert_eq!(fixture.cache_metrics().admissions, 0);
    }

    #[tokio::test]
    async fn transaction_trace_emits_hit_miss_admission_and_eviction_metrics() {
        let mut limits = config();
        limits.max_entries = 1;
        let fixture = Fixture::new(limits);
        seed(&fixture, "/first", "public, max-age=60").await;
        assert_eq!(
            seed(&fixture, "/second", "public, max-age=60").await,
            CacheAdmission::Stored { evicted: 1 }
        );
        assert!(matches!(
            fixture
                .transaction("/second", HeaderMap::new())
                .start()
                .await,
            CacheStart::Hit(_)
        ));

        let metrics = fixture.cache_metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.admissions, 2);
        assert_eq!(metrics.evictions, 1);
    }
}
