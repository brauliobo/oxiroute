use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
    header::{
        ACCEPT_RANGES, ALLOW, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE,
        CONTENT_TYPE, DATE, ETAG, HOST, IF_MATCH, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE,
        IF_UNMODIFIED_SINCE, LAST_MODIFIED, LOCATION, RANGE, RETRY_AFTER, SERVER, TRAILER,
        TRANSFER_ENCODING, WWW_AUTHENTICATE,
    },
    uri::Authority,
};
use log::{debug, warn};
use oxiroute_acme::ChallengeStore;
use oxiroute_cache::{CachedResponse, ResponseTiming};
use oxiroute_config::{
    HttpGzipMinimumVersion, HttpProxyPathRewrite, HttpRetryTarget, HttpRetryTrigger,
    HttpUpstreamHost, is_unambiguous_http_path,
};
use pingora::{
    Error, ErrorSource, ErrorType,
    modules::http::{HttpModule, HttpModuleBuilder, HttpModules, Module},
    protocols::Digest,
    protocols::http::{
        ServerSession,
        compression::{Algorithm, ResponseCompressionCtx},
    },
    proxy::{FailToProxy, PreparedUpstreamRequest, ProxyHttp, Session, UpstreamCancellation},
    upstreams::peer::HttpPeer,
};

use crate::routing::{EndpointObservation, QueueTag};
use crate::{
    GenerationReference, HealthFailure, HttpOperationResult, HttpServicePlan, ListenerMetrics,
    RuntimeEndpoint, RuntimeGeneration, RuntimeReferenceKind,
    http::action::{
        AccessLog, AffinityRequest, HttpActionPlan, HttpGzipPlan, HttpRoutePlan, ProxyActionPlan,
        ProxyPolicyPlan, RedirectLocationPlan, StaticErrorTarget, StaticFile,
        StaticRequestDecision, StaticServeError, StaticTarget,
    },
    http::cache::{
        CacheFailureClass, CacheRequest, CacheStart, CacheStartFailure, CacheTransaction,
        HttpCachePlan,
    },
    http::policy::{
        RedirectContext, RequestHeaderDecision, RequestPolicyContext, RequestPolicyError,
        ResponseHeaderDecision, ResponsePolicyError, decide_request_header,
        decide_response_headers, expand_redirect_location, nginx_request_host,
        normalized_redirect_host, normalized_request_host,
    },
    upstream_peer::{
        POOL_UNAVAILABLE, POOL_UNAVAILABLE_HEADER, SelectedEndpoint, UnavailableResponse,
        UpstreamPlan, enforce_http_version, is_pool_unavailable, validate_tls_connection,
    },
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use crate::http::evacuation::{Cancellation, EVACUATED, LeftRunning, is_evacuated};
use crate::http::inflight::{self, InflightArrival, InflightGuard, Phase, bounded_path};
use crate::http::request_id::{REQUEST_ID_HEADER, RequestIdBuilder};
use crate::observability::upstream_stats::{AttemptOutcome, EvacuationResult, ServerStats};

pub struct HttpReverseProxy {
    challenge_store: Option<ChallengeStore>,
    generation: Option<Arc<RuntimeGeneration>>,
    metrics: ListenerMetrics,
    service: Arc<HttpServicePlan>,
}

impl HttpReverseProxy {
    #[must_use]
    pub fn new(service: Arc<HttpServicePlan>, metrics: ListenerMetrics) -> Self {
        Self {
            challenge_store: None,
            generation: None,
            metrics,
            service,
        }
    }

    #[must_use]
    pub fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }

    #[must_use]
    pub fn with_challenge_store(mut self, challenge_store: ChallengeStore) -> Self {
        self.challenge_store = Some(challenge_store);
        self
    }

    fn challenge_response(
        &self,
        session: &Session,
    ) -> Option<oxiroute_acme::ChallengeHttpResponse> {
        let store = self.challenge_store.as_ref()?;
        let request = session.req_header();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        store.route(request.method.as_str(), request.uri.path(), now)
    }
}

const QUEUE_POSITION_HEADER: HeaderName = HeaderName::from_static("x-oxiroute-queue-position");
const QUEUE_WAIT_HEADER: HeaderName = HeaderName::from_static("x-oxiroute-queue-wait-ms");

#[expect(
    clippy::struct_excessive_bools,
    reason = "retry and cache state are independent protocol decisions"
)]
pub struct HttpRequestContext {
    listener: ListenerMetrics,
    observed_received: u64,
    observed_sent: u64,
    authority: Option<Authority>,
    attempted_upstreams: Vec<String>,
    pool: Option<Arc<UpstreamPlan>>,
    route: Option<Arc<HttpRoutePlan>>,
    selected: Option<SelectedEndpoint>,
    selected_observation: Option<EndpointObservation>,
    selected_upstream_host: Option<HeaderValue>,
    request_header_decisions: Box<[RequestHeaderDecision]>,
    connection_retryable: bool,
    replay_retryable: bool,
    evacuation_pin: Option<LeftRunning>,
    evacuations: usize,
    evacuated_from: Vec<Arc<ServerStats>>,
    retry_server: Option<String>,
    queue_tag: QueueTag,
    retry_delay_pending: bool,
    prior_attempts: usize,
    rescued: bool,
    retry_prepaid: bool,
    response_status_override: Option<u16>,
    response_header_overrides: Vec<(HeaderName, HeaderValue)>,
    started_at: Instant,
    total_deadline: Instant,
    client_deadline: Option<Instant>,
    deadline: Instant,
    operation_result: Option<HttpOperationResult>,
    websocket_reference: Option<GenerationReference>,
    generation: Option<Arc<RuntimeGeneration>>,
    cache_transaction: Option<CacheTransaction>,
    cache_capture: Option<CacheCapture>,
    response_buffer: Option<ResponseBuffer>,
    cache_response_handled: bool,
    trace: AttemptTrace,
    inflight: Option<InflightGuard>,
}

/// What the access log and per-server counters need to know about the attempts of one request.
#[derive(Default)]
struct AttemptTrace {
    servers: Vec<String>,
    failure_reason: Option<&'static str>,
    budget_spent: std::sync::atomic::AtomicBool,
    queue_wait: Duration,
    queue_position: u64,
    attempt_started: Option<Instant>,
    connect: Option<Duration>,
    connected_at: Option<Instant>,
    header: Option<Duration>,
    stats: Option<Arc<ServerStats>>,
    responded: bool,
    pending_outcome: Option<AttemptOutcome>,
}

impl AttemptTrace {
    /// Counts the attempt on its server and forgets the server until the next selection.
    fn finish(&mut self, outcome: AttemptOutcome, status: Option<u16>) {
        if let Some(stats) = self.stats.take() {
            stats.record_attempt(outcome);
            stats.record_response(status);
        }
    }
}

struct CacheCapture {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    tags: Vec<Bytes>,
    timing: ResponseTiming,
    complete: bool,
    admissible: bool,
}

struct ResponseBuffer {
    limit: usize,
    expected_length: usize,
    body: Vec<u8>,
}

impl HttpRequestContext {
    fn enter_phase(&self, phase: Phase) {
        if let Some(inflight) = &self.inflight {
            inflight.set_phase(phase);
        }
    }

    fn register_inflight(&mut self, session: &Session, access_log: Option<&Arc<AccessLog>>) {
        let request = session.req_header();
        let header = |name| {
            request
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.chars().take(inflight::MAX_PATH_BYTES).collect())
        };
        self.inflight = Some(
            inflight::registry().register(InflightArrival {
                request_id: header(&REQUEST_ID_HEADER).unwrap_or_default(),
                listener: self.listener.name().to_owned(),
                method: request.method.as_str().to_owned(),
                host: header(&HOST),
                path: bounded_path(request.uri.path()),
                client_ip: session
                    .client_addr()
                    .and_then(|address| address.as_inet())
                    .map(|address| address.ip().to_string()),
                deadline: self.deadline.saturating_duration_since(self.started_at),
                access_log: access_log.cloned(),
            }),
        );
    }

    fn observe(&mut self, session: &Session) {
        if let Some(inflight) = &self.inflight {
            inflight.progress();
        }
        observe_counter(
            &self.listener,
            session.body_bytes_read(),
            &mut self.observed_received,
            true,
        );
        observe_counter(
            &self.listener,
            session.body_bytes_sent(),
            &mut self.observed_sent,
            false,
        );
    }

    /// Servers tried over the whole request, including those of a rescue pool already left.
    fn attempt_count(&self) -> usize {
        self.prior_attempts + self.tried_servers()
    }

    fn has_attempted(&self) -> bool {
        self.prior_attempts + self.attempted_upstreams.len() > 0
    }

    /// Servers tried in the current pool. An evacuated attempt did not fail, so it is not a try.
    fn tried_servers(&self) -> usize {
        self.attempted_upstreams.len() - self.evacuations
    }

    /// Moves a rescued request to the route's rescue pool the first time; each pool tries a server once.
    fn begin_rescue(&mut self, status: u16) {
        let entering = rescue_pool_to_enter(self, true).cloned();
        self.rescued = true;
        if let Some(rescue_pool) = entering {
            self.prior_attempts += self.tried_servers();
            self.evacuations = 0;
            self.attempted_upstreams.clear();
            self.pool = Some(rescue_pool);
        }
        let route = proxy_route(self).route_id.clone();
        debug!(
            "rescuing route {route} status {status} after {} tries",
            self.attempt_count()
        );
        self.listener.record_rescue(&route, status);
    }

    /// A failed attempt is counted now when the request moves on, otherwise when the request ends.
    fn settle_attempt(&mut self, error: &Error, retried: bool) {
        let outcome = attempt_outcome(error);
        if retried {
            self.trace.finish(outcome, None);
        } else {
            self.trace.pending_outcome = Some(outcome);
        }
    }

    /// Settles the last attempt and adds what the finished request sent and received to its server.
    fn account_final_attempt(&mut self, session: &Session) {
        if self.trace.responded {
            if let (Some(stats), Some(connected)) = (&self.trace.stats, self.trace.connected_at) {
                let bytes = |count: usize| u64::try_from(count).unwrap_or(u64::MAX);
                stats.record_request_duration(connected.elapsed());
                stats.record_bytes(
                    bytes(session.body_bytes_read()),
                    bytes(session.body_bytes_sent()),
                );
            }
        } else {
            let fallback = match self.trace.failure_reason {
                Some("client_closed") => AttemptOutcome::ClientClosed,
                _ => AttemptOutcome::ProtocolError,
            };
            let outcome = self.trace.pending_outcome.take().unwrap_or(fallback);
            self.trace.finish(outcome, None);
        }
    }

    /// The attempt produced the response the client gets.
    fn note_response(&mut self, status: u16) {
        let trace = &mut self.trace;
        trace.responded = true;
        trace.header = trace.connected_at.map(|connected| connected.elapsed());
        if let Some(stats) = &trace.stats {
            stats.record_attempt(AttemptOutcome::Ok);
            stats.record_response(Some(status));
            if let Some(header) = trace.header {
                stats.record_header_duration(header);
            }
        }
        if let (Some(header), Some(observation), Some(route), 200..=299) = (
            trace.header,
            &self.selected_observation,
            &self.route,
            status,
        ) {
            observation.record_latency(&route.route_id, header);
        }
    }

    fn note_queue_wait(&mut self) {
        if let Some(selected) = &self.selected {
            selected.end_queue_wait();
            self.trace.queue_wait += selected.queue_wait();
            self.trace.queue_position = self.trace.queue_position.max(selected.queue_position());
        }
    }

    /// Tells the client how long it waited for capacity, when its route opted in.
    fn stamp_queue_headers(
        &self,
        response: &mut pingora::http::ResponseHeader,
    ) -> pingora::Result<()> {
        if !proxy_policy(self).queue_headers || self.trace.queue_position == 0 {
            return Ok(());
        }
        response.insert_header(QUEUE_POSITION_HEADER, self.trace.queue_position.to_string())?;
        response.insert_header(
            QUEUE_WAIT_HEADER,
            self.trace.queue_wait.as_millis().to_string(),
        )?;
        Ok(())
    }

    fn spend_retry_budget(&self) -> bool {
        let spent = proxy_policy(self)
            .retry_budget
            .as_ref()
            .is_none_or(|budget| budget.try_spend(Instant::now()));
        if !spent {
            self.trace.budget_spent.store(true, Ordering::Relaxed);
        }
        spent
    }

    fn release_lease(&mut self) {
        self.note_queue_wait();
        self.selected.take();
        self.selected_observation.take();
    }

    fn detach_lease(&mut self) {
        self.note_queue_wait();
        self.selected.take();
    }

    fn record_passive_failure(&mut self, failure: HealthFailure) {
        let observation = self
            .selected_observation
            .take()
            .or_else(|| self.selected.as_ref().map(SelectedEndpoint::observation));
        if let Some(observation) = observation {
            observation.record_passive_failure(failure);
        }
    }

    fn record_passive_success(&self) {
        if let Some(observation) = &self.selected_observation {
            observation.record_passive_success();
        }
    }

    fn adopt_downstream_deadline(&mut self, session: &Session) {
        self.client_deadline = [session.get_read_timeout(), session.get_write_timeout()]
            .into_iter()
            .flatten()
            .map(|timeout| self.started_at + timeout)
            .chain(self.client_deadline)
            .min();
        self.refresh_deadline();
    }

    /// A route total shortens the service deadline; it never extends it.
    fn adopt_route_total(&mut self, total: Option<Duration>) {
        if let Some(total) = total {
            self.total_deadline = self.total_deadline.min(self.started_at + total);
            self.refresh_deadline();
        }
    }

    fn refresh_deadline(&mut self) {
        self.deadline = self.client_deadline.map_or(self.total_deadline, |client| {
            client.min(self.total_deadline)
        });
        if let Some(inflight) = &self.inflight {
            inflight.set_deadline(self.deadline.saturating_duration_since(self.started_at));
        }
    }

    fn client_deadline_expired(&self) -> bool {
        self.client_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn remaining(&self, upstream: bool) -> pingora::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(if upstream {
                Error::new_up(ErrorType::ReadTimedout)
            } else {
                Error::new_down(ErrorType::ReadTimedout)
            });
        }
        Ok(remaining)
    }

    fn allow_downstream_drain(&self, session: &mut Session) {
        if let Ok(remaining) = self.remaining(false) {
            session.set_total_drain_timeout(Some(remaining));
            session.set_close_on_response_before_downstream_finish(false);
            session.set_keepalive(Some(0));
            self.close_when_retired(session);
        }
    }

    /// A replaced generation finishes the request in progress and then closes the connection.
    fn close_when_retired(&self, session: &mut Session) {
        if self
            .generation
            .as_ref()
            .is_some_and(|generation| generation.retired())
        {
            session.set_keepalive(None);
        }
    }
}

impl Drop for HttpRequestContext {
    fn drop(&mut self) {
        if self.operation_result.is_some() {
            return;
        }
        if let Some(transaction) = self.cache_transaction.as_mut() {
            transaction.cancel();
        }
        let timed_out = Instant::now() >= self.deadline;
        let result = if timed_out {
            HttpOperationResult::Timeout
        } else {
            HttpOperationResult::Cancelled
        };
        self.operation_result = Some(result);
        let _ = self
            .listener
            .record_http_operation(result, self.started_at.elapsed());
    }
}

mod access_record;
mod action;
mod cache;
mod compression;
mod evacuation;
mod proxy_http;
mod request;
mod response;
mod retry;
mod static_files;

pub(crate) use cache::*;
pub(crate) use request::*;
pub(crate) use response::*;
#[allow(clippy::wildcard_imports)]
use {action::*, compression::*, retry::*, static_files::*};

#[cfg(test)]
mod tests;
