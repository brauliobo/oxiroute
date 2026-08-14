use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
    header::{
        ACCEPT_RANGES, ALLOW, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE,
        CONTENT_TYPE, ETAG, HOST, IF_MATCH, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE,
        IF_UNMODIFIED_SINCE, LAST_MODIFIED, LOCATION, RANGE, SERVER, TRAILER, TRANSFER_ENCODING,
        WWW_AUTHENTICATE,
    },
    uri::Authority,
};
use log::warn;
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
    protocols::http::compression::{Algorithm, ResponseCompressionCtx},
    proxy::{FailToProxy, PreparedUpstreamRequest, ProxyHttp, Session},
    upstreams::peer::HttpPeer,
};

use crate::routing::EndpointObservation;
use crate::{
    GenerationReference, HealthFailure, HttpOperationResult, HttpServicePlan, ListenerMetrics,
    RuntimeEndpoint, RuntimeGeneration, RuntimeReferenceKind,
    http_action::{
        HttpActionPlan, HttpGzipPlan, HttpRoutePlan, ProxyPolicyPlan, RedirectLocationPlan,
        StaticErrorTarget, StaticFile, StaticRequestDecision, StaticServeError, StaticTarget,
    },
    http_cache::{
        CacheFailureClass, CacheRequest, CacheStart, CacheStartFailure, CacheTransaction,
        HttpCachePlan,
    },
    http_policy::{
        RedirectContext, RequestHeaderDecision, RequestPolicyContext, RequestPolicyError,
        ResponseHeaderDecision, ResponsePolicyError, decide_request_header,
        decide_response_headers, expand_redirect_location, nginx_request_host,
        normalized_redirect_host, normalized_request_host,
    },
    upstream_peer::{
        SelectedEndpoint, UpstreamPlan, enforce_http_version, validate_tls_connection,
    },
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

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
    retry_server: Option<String>,
    retry_delay_pending: bool,
    response_status_override: Option<u16>,
    response_header_overrides: Vec<(HeaderName, HeaderValue)>,
    started_at: Instant,
    deadline: Instant,
    operation_result: Option<HttpOperationResult>,
    websocket_reference: Option<GenerationReference>,
    cache_transaction: Option<CacheTransaction>,
    cache_capture: Option<CacheCapture>,
    response_buffer: Option<ResponseBuffer>,
    cache_response_handled: bool,
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
    fn observe(&mut self, session: &Session) {
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

    fn release_lease(&mut self) {
        self.selected.take();
        self.selected_observation.take();
    }

    fn detach_lease(&mut self) {
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

    fn adopt_downstream_deadline(&mut self, session: &Session) {
        for timeout in [session.get_read_timeout(), session.get_write_timeout()]
            .into_iter()
            .flatten()
        {
            self.deadline = std::cmp::min(self.deadline, self.started_at + timeout);
        }
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

#[async_trait]
impl ProxyHttp for HttpReverseProxy {
    type CTX = HttpRequestContext;

    fn init_downstream_modules(&self, modules: &mut HttpModules) {
        if let Some(gzip) = self.service.gzip() {
            modules.add_module(Box::new(ConfiguredCompressionBuilder {
                gzip: Arc::clone(gzip),
            }));
        }
    }

    fn new_ctx(&self) -> Self::CTX {
        let started_at = Instant::now();
        HttpRequestContext {
            listener: self.metrics.clone(),
            observed_received: 0,
            observed_sent: 0,
            authority: None,
            attempted_upstreams: Vec::new(),
            pool: None,
            route: None,
            selected: None,
            selected_observation: None,
            selected_upstream_host: None,
            request_header_decisions: Box::new([]),
            connection_retryable: false,
            replay_retryable: false,
            retry_server: None,
            retry_delay_pending: false,
            response_status_override: None,
            response_header_overrides: Vec::new(),
            started_at,
            deadline: started_at + self.service.upstream_io_timeout(),
            operation_result: None,
            websocket_reference: None,
            cache_transaction: None,
            cache_capture: None,
            response_buffer: None,
            cache_response_handled: false,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        ctx.adopt_downstream_deadline(session);
        let remaining = ctx.remaining(false)?;
        let write_timeout = session
            .get_write_timeout()
            .map_or(remaining, |timeout| timeout.min(remaining));
        if session.is_http2() {
            let read_timeout = session
                .get_read_timeout()
                .map_or(remaining, |timeout| timeout.min(remaining));
            session.set_read_timeout(Some(read_timeout));
        }
        session.set_total_drain_timeout(Some(remaining));
        session.set_write_timeout(Some(write_timeout));
        if let Some(response) = self.challenge_response(session) {
            let head = session.req_header().method == Method::HEAD;
            write_local_response(
                session,
                response.status,
                &[
                    (
                        CONTENT_TYPE,
                        HeaderValue::from_static(response.content_type),
                    ),
                    (
                        HeaderName::from_static("cache-control"),
                        HeaderValue::from_static(response.cache_control),
                    ),
                ],
                Bytes::from(response.body),
                head,
            )
            .await?;
            ctx.allow_downstream_drain(session);
            return Ok(true);
        }
        session.set_automatic_response_headers(self.service.automatic_response_headers());
        let (authority, content_length, method, uri, upgrade) = {
            let request = session.req_header();
            let authority = request_authority(request);
            let content_length = content_length(&request.headers);
            (
                authority,
                content_length,
                request.method.clone(),
                request.uri.clone(),
                session.is_upgrade_req(),
            )
        };

        if upgrade && self.generation.is_some() {
            // The upgraded stream outlives its HTTP request admission, so retain a distinct
            // WebSocket protocol reference until Pingora drops the request context.
            let Some(reference) = self
                .generation
                .as_ref()
                .and_then(|generation| generation.begin_reference(RuntimeReferenceKind::WebSocket))
            else {
                session.respond_error(503).await?;
                ctx.allow_downstream_drain(session);
                return Ok(true);
            };
            ctx.websocket_reference = Some(reference);
        }

        let Ok(authority) = authority else {
            session.respond_error(400).await?;
            return Ok(true);
        };
        if content_length.is_err() || !is_unambiguous_http_path(uri.path()) {
            session.respond_error(400).await?;
            return Ok(true);
        }
        let Some(route) = self.service.select_route(authority.as_ref(), &uri, &method) else {
            session
                .respond_error_with_body(404, Bytes::from_static(b"route not found\n"))
                .await?;
            ctx.allow_downstream_drain(session);
            return Ok(true);
        };
        ctx.authority = authority;
        ctx.route = Some(Arc::clone(&route));
        session.set_total_drain_timeout(Some(ctx.remaining(false)?.min(route.policy.read_timeout)));
        if content_length
            .expect("checked content length")
            .is_some_and(|length| route.policy.exceeds_body_limit(length))
        {
            session.respond_error(413).await?;
            ctx.allow_downstream_drain(session);
            return Ok(true);
        }
        if let Some(access) = &route.access
            && !access.authorizes(&session.req_header().headers).await
        {
            write_local_response(
                session,
                401,
                &[(WWW_AUTHENTICATE, access.challenge().clone())],
                Bytes::new(),
                false,
            )
            .await?;
            ctx.allow_downstream_drain(session);
            return Ok(true);
        }

        match pingora_request_header_decisions(session, &route, ctx.authority.as_ref()) {
            Ok(decisions) => ctx.request_header_decisions = decisions,
            Err(RequestPolicyError::SourceTooLarge) => {
                session.respond_error(431).await?;
                ctx.allow_downstream_drain(session);
                return Ok(true);
            }
            Err(error) => return Err(pingora_request_policy_error(error).into()),
        }
        if let HttpActionPlan::Proxy(proxy) = &route.action
            && let Some(cache) = &proxy.policy.cache
        {
            if method.as_str().eq_ignore_ascii_case("PURGE") {
                if cache.purge_access.is_some() {
                    let response_sent =
                        cache_purge_filter(session, ctx, Arc::clone(cache), &method, &uri).await?;
                    if response_sent {
                        ctx.allow_downstream_drain(session);
                    }
                    return Ok(response_sent);
                }
            } else if cache.allows_method(&method)
                && !upgrade
                && cache_request_filter(session, ctx, Arc::clone(cache), &method, &uri).await?
            {
                ctx.allow_downstream_drain(session);
                return Ok(true);
            }
        }
        let response_sent =
            execute_route_action(&self.service, session, ctx, route, &method, uri).await?;
        if response_sent {
            ctx.allow_downstream_drain(session);
        }
        Ok(response_sent)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let remaining = ctx.remaining(true)?;
        let Some(pool) = &ctx.pool else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        if ctx.selected.is_none() {
            if ctx.retry_delay_pending {
                let delay = proxy_policy(ctx).retry_delay;
                if delay > remaining {
                    return Err(Error::new_up(ErrorType::ReadTimedout));
                }
                tokio::time::sleep(delay).await;
                ctx.retry_delay_pending = false;
            }
            let selected = if let Some(server) = ctx.retry_server.take() {
                pool.select_server_endpoint(&server).await?
            } else {
                pool.select_endpoint(&ctx.attempted_upstreams).await?
            };
            ctx.selected_upstream_host = selected_upstream_host(
                selected.endpoint(),
                proxy_policy(ctx).upstream_host.clone(),
                ctx.authority.as_ref(),
            )?;
            ctx.attempted_upstreams
                .push(selected.server_name().to_owned());
            ctx.selected_observation = Some(selected.observation());
            ctx.selected = Some(selected);
        }
        let remaining = ctx.remaining(true)?;
        let route_policy = proxy_route(ctx).policy;
        let peer = ctx
            .selected
            .as_mut()
            .expect("selected endpoint initialized")
            .prepare_peer_with_timeouts(
                pool,
                pool.connect_timeout(route_policy.connect_timeout)
                    .min(remaining),
                pool.server_timeout(route_policy.read_timeout)
                    .min(remaining),
                pool.server_timeout(route_policy.write_timeout)
                    .min(remaining),
            )
            .await?;
        Ok(Box::new(peer))
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut error: Box<Error>,
    ) -> Box<Error> {
        let has_address_fallback = ctx
            .selected
            .as_ref()
            .is_some_and(SelectedEndpoint::has_address_fallback);
        if !has_address_fallback {
            if let Some(failure) = passive_failure_for_error(&error) {
                ctx.record_passive_failure(failure);
            }
            ctx.release_lease();
        }
        let policy = proxy_policy(ctx);
        let has_budget = ctx.attempted_upstreams.len() <= usize::from(policy.max_retries);
        let retry_target = policy.target_for_retry(ctx.attempted_upstreams.len());
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => ctx.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => ctx
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
        };
        let trigger = connect_retry_trigger(&error);
        let retry = should_retry_connection(
            has_address_fallback,
            ctx.connection_retryable
                && trigger.is_some_and(|trigger| policy.retries_on(trigger))
                && has_budget
                && target_available
                && ctx.remaining(true).is_ok(),
        );
        error.set_retry(retry);
        if retry {
            if !has_address_fallback {
                if retry_target == HttpRetryTarget::SameServer {
                    ctx.retry_server = ctx.attempted_upstreams.last().cloned();
                }
                ctx.retry_delay_pending = retry_target == HttpRetryTarget::SameServer;
            }
            ctx.listener.record_retry_attempt();
        }
        error
    }

    fn error_while_proxy(
        &self,
        _peer: &HttpPeer,
        session: &mut Session,
        mut error: Box<Error>,
        ctx: &mut Self::CTX,
        _client_reused: bool,
    ) -> Box<Error> {
        if let Some(failure) = passive_failure_for_error(&error) {
            ctx.record_passive_failure(failure);
        }
        ctx.release_lease();
        let policy = proxy_policy(ctx);
        let has_budget = ctx.attempted_upstreams.len() <= usize::from(policy.max_retries);
        let retry_target = policy.target_for_retry(ctx.attempted_upstreams.len());
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => ctx.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => ctx
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
        };
        let retry = ctx.replay_retryable
            && request_body_replayable(session)
            && response_is_retryable(session)
            && retryable_upstream_error(&error, session, policy)
            && has_budget
            && target_available
            && ctx.remaining(true).is_ok();
        error.set_retry(retry);
        if retry {
            if retry_target == HttpRetryTarget::SameServer {
                ctx.retry_server = ctx.attempted_upstreams.last().cloned();
            }
            ctx.retry_delay_pending = retry_target == HttpRetryTarget::SameServer;
            ctx.listener.record_retry_attempt();
        }
        error
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        error: &Error,
        ctx: &mut Self::CTX,
    ) -> FailToProxy {
        if ctx.remaining(false).is_err() {
            return FailToProxy {
                error_code: 0,
                can_reuse_downstream: false,
            };
        }
        if ctx.cache_response_handled {
            return FailToProxy {
                error_code: session
                    .response_written()
                    .map_or(0, |response| response.status.as_u16()),
                can_reuse_downstream: false,
            };
        }
        if error.esource() == &pingora::ErrorSource::Upstream {
            let stale = if let Some(transaction) = ctx.cache_transaction.as_mut() {
                transaction
                    .stale_response(CacheFailureClass::Upstream)
                    .await
            } else {
                None
            };
            if let Some(stale) = stale
                && write_cached_response(session, &stale).await.is_ok()
            {
                if let Some(transaction) = ctx.cache_transaction.as_mut() {
                    transaction.complete_without_store();
                    transaction.record_hit();
                }
                ctx.cache_response_handled = true;
                return FailToProxy {
                    error_code: stale.status.as_u16(),
                    can_reuse_downstream: false,
                };
            }
        }
        let code = proxy_error_status(error);
        let nginx_server = ctx.route.as_ref().and_then(|route| match &route.action {
            HttpActionPlan::Proxy(proxy) => proxy.policy.nginx_error_server.clone(),
            HttpActionPlan::Fixed(_) | HttpActionPlan::Redirect(_) | HttpActionPlan::Static(_) => {
                None
            }
        });
        if code == 502
            && error.esource() == &ErrorSource::Upstream
            && let Some(server) = nginx_server
        {
            let headers = [
                (SERVER, server.clone()),
                (CONTENT_TYPE, HeaderValue::from_static("text/html")),
            ];
            let body = nginx_error_body(502, "Bad Gateway", &server);
            if let Err(write_error) = write_local_response(
                session,
                code,
                &headers,
                body,
                session.req_header().method == Method::HEAD,
            )
            .await
            {
                warn!("failed to send nginx proxy error response downstream: {write_error}");
            }
        } else if code > 0
            && let Err(write_error) = session.respond_error(code).await
        {
            warn!("failed to send error response downstream: {write_error}");
        }
        FailToProxy {
            error_code: code,
            can_reuse_downstream: false,
        }
    }

    async fn request_body_filter(
        &self,
        session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        ctx.observe(session);
        if session.was_upgraded() {
            return Ok(());
        }
        let remaining = ctx.remaining(false)?;
        if session.is_http2() {
            if let Some(read_timeout) = session.get_read_timeout() {
                session.set_read_timeout(Some(read_timeout.min(remaining)));
            } else {
                session.set_read_timeout(Some(remaining));
            }
        }
        if u64::try_from(session.body_bytes_read()).is_ok_and(|bytes| {
            ctx.route
                .as_ref()
                .is_some_and(|route| route.policy.exceeds_body_limit(bytes))
        }) {
            ctx.release_lease();
            return Err(Error::new_down(ErrorType::HTTPStatus(413)));
        }
        Ok(())
    }

    fn request_body_buffer_limit(&self, ctx: &Self::CTX) -> Option<usize> {
        ctx.route
            .as_ref()
            .and_then(|route| route.policy.request_body_buffer_limit())
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        ctx.remaining(true)?;
        let result = enforce_http_version(
            ctx.pool.as_deref().and_then(UpstreamPlan::tls),
            upstream_request.version,
        );
        if result.is_err() {
            if let Err(error) = &result
                && let Some(failure) = passive_failure_for_error(error)
            {
                ctx.record_passive_failure(failure);
            }
            ctx.release_lease();
        }
        result?;
        if upstream_request.version == http::Version::HTTP_10 {
            upstream_request.set_version(http::Version::HTTP_11);
        }
        if let Some(rewrite) = &proxy_policy(ctx).upstream_path_rewrite {
            rewrite_upstream_path(&mut upstream_request.uri, rewrite)?;
        }
        if ctx
            .cache_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.request().method == Method::HEAD)
        {
            upstream_request.method = Method::GET;
        }
        if let Some(transaction) = ctx.cache_transaction.as_ref() {
            transaction.apply_validators(&mut upstream_request.headers);
        }
        if let Some(host) = &ctx.selected_upstream_host {
            upstream_request.insert_header(HOST, host.clone())?;
        } else {
            upstream_request.remove_header(&HOST);
        }
        if ctx.pool.as_ref().is_some_and(|pool| {
            pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
        }) {
            upstream_request.insert_header(CONNECTION, "close")?;
        }
        apply_request_header_mutations(upstream_request, ctx)?;
        Ok(())
    }

    async fn prepare_upstream_request(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<PreparedUpstreamRequest> {
        ctx.remaining(true)?;
        let result = enforce_http_version(
            ctx.pool.as_deref().and_then(UpstreamPlan::tls),
            session.req_header().version,
        );
        if result.is_err() {
            if let Err(error) = &result
                && let Some(failure) = passive_failure_for_error(error)
            {
                ctx.record_passive_failure(failure);
            }
            ctx.release_lease();
        }
        result?;

        if !upstream_request_requires_mutation(session, ctx) {
            return Ok(PreparedUpstreamRequest::Borrowed);
        }

        let mut upstream_request = session.req_header().clone();
        self.upstream_request_filter(session, &mut upstream_request, ctx)
            .await?;
        Ok(PreparedUpstreamRequest::Owned(Box::new(upstream_request)))
    }

    #[allow(clippy::too_many_lines)]
    async fn upstream_response_filter(
        &self,
        session: &mut Session,
        response: &mut pingora::http::ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        let remaining = ctx.remaining(true)?;
        let write_timeout = session
            .get_write_timeout()
            .map_or(remaining, |timeout| timeout.min(remaining));
        session.set_write_timeout(Some(write_timeout));
        let retries_on_status = proxy_policy(ctx).retries_on_status(response.status.as_u16());
        if retries_on_status && response_status_retryable(session, ctx) {
            let mut error = Error::new_up(ErrorType::HTTPStatus(response.status.as_u16()));
            error.set_retry(true);
            return Err(error);
        }
        if retries_on_status {
            ctx.record_passive_failure(HealthFailure::UnexpectedStatus);
        }
        let had_uncacheable_framing = response.headers.contains_key(TRANSFER_ENCODING)
            || response.headers.contains_key(TRAILER);
        let remove_hop_by_hop = response.status != http::StatusCode::SWITCHING_PROTOCOLS;
        if let Some(status) = ctx.response_status_override {
            response.set_status(status)?;
        }
        for (name, value) in &ctx.response_header_overrides {
            response.append_header(name.clone(), value.clone())?;
        }
        apply_response_policy(response, proxy_policy(ctx), remove_hop_by_hop)?;
        ctx.response_buffer = None;
        if ctx
            .cache_transaction
            .as_ref()
            .is_some_and(CacheTransaction::is_revalidation)
        {
            if response.status == StatusCode::NOT_MODIFIED {
                return finish_cache_revalidation(session, ctx, response).await;
            }
            if response.status.is_server_error() {
                let stale = if let Some(transaction) = ctx.cache_transaction.as_mut() {
                    transaction
                        .stale_response(CacheFailureClass::Upstream)
                        .await
                } else {
                    None
                };
                if let Some(stale) = stale {
                    return finish_cache_stale_response(session, ctx, stale).await;
                }
            }
        }
        if let Some(limit) = proxy_route(ctx).policy.response_body_buffer_limit()
            && !session.is_upgrade_req()
            && session.req_header().method != Method::HEAD
            && !response.status.is_informational()
            && !matches!(
                response.status,
                StatusCode::NO_CONTENT
                    | StatusCode::RESET_CONTENT
                    | StatusCode::NOT_MODIFIED
                    | StatusCode::SWITCHING_PROTOCOLS
            )
        {
            if had_uncacheable_framing {
                return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
            }
            let length = content_length(&response.headers)
                .map_err(|()| Error::new_up(ErrorType::InvalidHTTPHeader))?
                .and_then(|length| usize::try_from(length).ok())
                .ok_or_else(|| Error::new_up(ErrorType::InvalidHTTPHeader))?;
            if length > limit {
                return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
            }
            ctx.response_buffer = Some(ResponseBuffer {
                limit,
                expected_length: length,
                body: Vec::new(),
            });
        }
        if let Some(transaction) = &ctx.cache_transaction {
            let status = response.status;
            if !status.is_informational() && status != StatusCode::SWITCHING_PROTOCOLS {
                let complete = status == StatusCode::NO_CONTENT
                    || status == StatusCode::RESET_CONTENT
                    || response
                        .headers
                        .get(CONTENT_LENGTH)
                        .is_some_and(|value| value.as_bytes() == b"0");
                ctx.cache_capture = Some(CacheCapture {
                    status,
                    headers: response.headers.clone(),
                    body: Vec::new(),
                    tags: transaction
                        .surrogate_header()
                        .map_or_else(Vec::new, |header| {
                            response_surrogate_tags(&response.headers, header)
                        }),
                    timing: ResponseTiming {
                        request_started: transaction.request().request_started,
                        response_received: transaction.now(),
                        response_received_wall: SystemTime::now(),
                    },
                    complete,
                    admissible: !had_uncacheable_framing
                        && !response.headers.contains_key(TRANSFER_ENCODING)
                        && !response.headers.contains_key(TRAILER),
                });
            }
        }
        Ok(())
    }

    async fn connected_to_upstream(
        &self,
        _session: &mut Session,
        _reused: bool,
        _peer: &HttpPeer,
        #[cfg(unix)] _fd: std::os::unix::io::RawFd,
        #[cfg(windows)] _socket: std::os::windows::io::RawSocket,
        digest: Option<&Digest>,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        ctx.remaining(true)?;
        let result =
            validate_tls_connection(ctx.pool.as_deref().and_then(UpstreamPlan::tls), digest);
        if result.is_err() {
            if let Err(error) = &result
                && let Some(failure) = passive_failure_for_error(error)
            {
                ctx.record_passive_failure(failure);
            }
            ctx.release_lease();
        } else {
            // The Pingora socket digest now owns the lease until the physical connection closes.
            ctx.detach_lease();
        }
        result
    }

    fn response_body_filter(
        &self,
        session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Option<Duration>> {
        if session.was_upgraded() {
            ctx.observe(session);
            return Ok(None);
        }
        let remaining = ctx.remaining(false)?;
        if let Some(write_timeout) = session.get_write_timeout() {
            session.set_write_timeout(Some(write_timeout.min(remaining)));
        } else {
            session.set_write_timeout(Some(remaining));
        }
        ctx.observe(session);
        Ok(None)
    }

    fn upstream_response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Option<Duration>> {
        if !session.was_upgraded() {
            ctx.remaining(true)?;
        }
        if let Some(capture) = ctx.cache_capture.as_mut() {
            if session.was_upgraded() {
                capture.admissible = false;
            }
            if capture.admissible
                && let Some(data) = body.as_ref()
            {
                let limit = ctx
                    .cache_transaction
                    .as_ref()
                    .expect("cache capture has a transaction")
                    .max_body_bytes();
                if capture.body.len().saturating_add(data.len()) > limit {
                    capture.admissible = false;
                    capture.body.clear();
                } else {
                    capture.body.extend_from_slice(data);
                }
            }
            if end_of_stream {
                capture.complete = true;
            }
        }
        if session.was_upgraded() {
            return Ok(None);
        }
        let Some(mut buffer) = ctx.response_buffer.take() else {
            return Ok(None);
        };
        if let Some(data) = body.take() {
            let Some(new_length) = buffer.body.len().checked_add(data.len()) else {
                return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
            };
            if new_length > buffer.limit || new_length > buffer.expected_length {
                return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
            }
            buffer.body.extend_from_slice(&data);
        }
        if end_of_stream {
            if buffer.body.len() != buffer.expected_length {
                return Err(Error::new_up(ErrorType::InvalidHTTPHeader));
            }
            *body = (!buffer.body.is_empty()).then(|| Bytes::from(buffer.body));
        } else {
            *body = None;
            ctx.response_buffer = Some(buffer);
        }
        Ok(None)
    }

    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        if error.is_none() {
            finish_cache_fill(ctx).await;
        } else {
            ctx.cache_capture = None;
            if let Some(transaction) = ctx.cache_transaction.as_mut() {
                transaction.complete_without_store();
            }
        }
        let response_status = session
            .response_written()
            .map(|response| response.status.as_u16());
        if let Some(error) = error {
            warn!(
                "HTTP request failed with {:?} from {:?}",
                error.etype(),
                error.esource()
            );
        }
        if ctx.operation_result.is_none() {
            let result = classify_http_result(
                (!ctx.cache_response_handled).then_some(error).flatten(),
                response_status,
            );
            ctx.operation_result = Some(result);
            if let Err(metric_error) = ctx
                .listener
                .record_http_operation(result, ctx.started_at.elapsed())
            {
                warn!("could not account for HTTP operation metrics: {metric_error}");
            }
        }
        ctx.observe(session);
        ctx.release_lease();
        if let Some(access_log) = self.service.access_log() {
            let request = session.req_header();
            let event = serde_json::json!({
                "timestampUnixMs": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
                "service": access_log.service(),
                "route": ctx.route.as_ref().map(|route| route.route_id.as_str()),
                "host": ctx.authority.as_ref().map(normalized_request_host),
                "method": request.method.as_str(),
                "status": response_status,
                "bytesReceived": session.body_bytes_read().to_string(),
                "bytesSent": session.body_bytes_sent().to_string(),
                "durationMs": ctx.started_at.elapsed().as_millis().to_string(),
                "clientIp": session.client_addr().and_then(|address| address.as_inet()).map(|address| address.ip().to_string()),
            });
            if let Err(error) = access_log.write(&event) {
                warn!("HTTP access log write failed: {error}");
            }
        }
    }
}

async fn finish_cache_revalidation(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    response: &pingora::http::ResponseHeader,
) -> pingora::Result<()> {
    let transaction = ctx
        .cache_transaction
        .as_mut()
        .expect("revalidation has a cache transaction");
    let timing = ResponseTiming {
        request_started: transaction.request().request_started,
        response_received: transaction.now(),
        response_received_wall: SystemTime::now(),
    };
    let cached = transaction
        .finish_revalidation(&response.headers, timing)
        .await;
    finish_cache_response(session, ctx, cached).await
}

async fn finish_cache_stale_response(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    response: CachedResponse,
) -> pingora::Result<()> {
    if let Some(transaction) = ctx.cache_transaction.as_mut() {
        transaction.complete_without_store();
    }
    finish_cache_response(session, ctx, response).await
}

async fn finish_cache_response(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    response: CachedResponse,
) -> pingora::Result<()> {
    ctx.cache_capture = None;
    ctx.cache_response_handled = true;
    ctx.cache_transaction
        .as_ref()
        .expect("cached response has a transaction")
        .record_hit();
    write_cached_response_conditionally(session, &response).await?;
    Err(Error::new_in(ErrorType::InternalError))
}

async fn cache_request_filter(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    cache: Arc<HttpCachePlan>,
    method: &Method,
    uri: &http::Uri,
) -> pingora::Result<bool> {
    let Some(authority) = ctx.authority.as_ref() else {
        return Ok(false);
    };
    let headers = session.req_header().headers.clone();
    if cache_request_bypasses_cache(&headers) {
        return Ok(false);
    }
    let scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let request = cache_request(cache.as_ref(), authority, method, uri, headers, scheme);
    match CacheTransaction::new(cache, request, ctx.listener.clone())
        .start()
        .await
    {
        CacheStart::Bypass(failure) => {
            if let CacheStartFailure::Lookup(error) = failure
                && !error.is_invalid_request()
            {
                warn!("cache lookup bypassed after validation failure: {error}");
            }
            Ok(false)
        }
        CacheStart::Hit(response) => {
            ctx.cache_response_handled = true;
            write_cached_response_conditionally(session, &response).await?;
            Ok(true)
        }
        CacheStart::OnlyIfCached => {
            session.respond_error(504).await?;
            Ok(true)
        }
        CacheStart::MissLeader(transaction) | CacheStart::RevalidationLeader(transaction) => {
            ctx.cache_transaction = Some(transaction);
            Ok(false)
        }
    }
}

fn cache_request(
    cache: &HttpCachePlan,
    authority: &Authority,
    method: &Method,
    uri: &http::Uri,
    headers: HeaderMap,
    scheme: &'static str,
) -> CacheRequest {
    CacheRequest {
        method: method.clone(),
        scheme,
        authority: authority.as_str().to_owned(),
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        headers,
        request_started: cache.cache.now(),
    }
}

async fn cache_purge_filter(
    session: &mut Session,
    _ctx: &mut HttpRequestContext,
    cache: Arc<HttpCachePlan>,
    method: &Method,
    uri: &http::Uri,
) -> pingora::Result<bool> {
    let Some(access) = cache.purge_access.as_ref() else {
        return Ok(false);
    };
    if !access.authorizes(&session.req_header().headers) {
        let head = session.req_header().method == Method::HEAD;
        write_local_response(
            session,
            401,
            &[(WWW_AUTHENTICATE, access.challenge().clone())],
            Bytes::new(),
            head,
        )
        .await?;
        return Ok(true);
    }
    let (authority_result, headers) = {
        let request = session.req_header();
        (request_authority(request), request.headers.clone())
    };
    let Ok(Some(authority)) = authority_result else {
        session.respond_error(400).await?;
        return Ok(true);
    };
    let scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let request = cache_request(
        cache.as_ref(),
        &authority,
        method,
        uri,
        headers.clone(),
        scheme,
    );
    let tag = cache
        .surrogate_header
        .as_ref()
        .and_then(|header| headers.get(header));
    let result = if let Some(value) = tag {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b'"'))
        {
            write_local_response(
                session,
                400,
                &[],
                Bytes::from_static(b"invalid cache purge tag\n"),
                false,
            )
            .await?;
            return Ok(true);
        }
        cache.cache.purge_tag(bytes).await
    } else {
        match cache.cache.base(&request) {
            Ok(base) => cache.cache.purge_base(&base).await,
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(result) => {
            let status = if result.entries == 0 { 404 } else { 200 };
            let head = session.req_header().method == Method::HEAD;
            write_local_response(
                session,
                status,
                &[(
                    HeaderName::from_static("cache-control"),
                    HeaderValue::from_static("private, no-store"),
                )],
                Bytes::new(),
                head,
            )
            .await?;
        }
        Err(error) => {
            warn!("cache purge failed: {error}");
            let status = if error.is_invalid_request() { 400 } else { 503 };
            session.respond_error(status).await?;
        }
    }
    Ok(true)
}

async fn finish_cache_fill(ctx: &mut HttpRequestContext) {
    let Some(capture) = ctx.cache_capture.take() else {
        if let Some(transaction) = ctx.cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        return;
    };
    let Some(transaction) = ctx.cache_transaction.as_mut() else {
        return;
    };
    let CacheCapture {
        status,
        headers,
        body,
        tags,
        timing,
        complete,
        admissible,
    } = capture;
    let tags_valid = transaction.cache_tags_within_limits(&tags);
    if !complete
        || !admissible
        || !tags_valid
        || !response_representation_valid(status, &headers, body.len())
    {
        transaction.complete_without_store();
        return;
    }
    let tag_refs = tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
    let prepared =
        transaction.prepare_response(status, &headers, Bytes::from(body), timing, &tag_refs);
    let Ok(entry) = prepared else {
        transaction.complete_without_store();
        return;
    };
    let _ = transaction.admit(entry).await;
}

fn response_representation_valid(status: StatusCode, headers: &HeaderMap, body_len: usize) -> bool {
    if status.is_informational()
        || status == StatusCode::SWITCHING_PROTOCOLS
        || status == StatusCode::NOT_MODIFIED
    {
        return false;
    }
    if matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT) && body_len != 0 {
        return false;
    }
    let mut lengths = headers.get_all(CONTENT_LENGTH).iter();
    let Some(length) = lengths.next() else {
        return true;
    };
    lengths.next().is_none()
        && length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            == Some(body_len)
}

fn response_surrogate_tags(headers: &HeaderMap, name: &HeaderName) -> Vec<Bytes> {
    headers
        .get_all(name)
        .iter()
        .flat_map(|value| {
            value
                .as_bytes()
                .split(u8::is_ascii_whitespace)
                .filter(|tag| !tag.is_empty())
                .map(Bytes::copy_from_slice)
        })
        .collect()
}

pub(crate) fn cache_request_bypasses_cache(headers: &HeaderMap) -> bool {
    headers.contains_key(RANGE)
        || headers.contains_key(IF_RANGE)
        || headers.contains_key(IF_MATCH)
        || headers.contains_key(IF_UNMODIFIED_SINCE)
}

async fn write_cached_response_conditionally(
    session: &mut Session,
    response: &CachedResponse,
) -> pingora::Result<()> {
    if cached_response_matches_condition(session, response) {
        let mut not_modified = response.clone();
        not_modified.status = StatusCode::NOT_MODIFIED;
        not_modified.body = Bytes::new();
        write_cached_response(session, &not_modified).await
    } else {
        write_cached_response(session, response).await
    }
}

fn cached_response_matches_condition(session: &Session, response: &CachedResponse) -> bool {
    let request = session.req_header();
    if let Some(if_none_match) = request.headers.get(IF_NONE_MATCH) {
        return cached_etag_list_matches(
            if_none_match.as_bytes(),
            response.headers.get(ETAG).map(HeaderValue::as_bytes),
        );
    }
    if !matches!(request.method, Method::GET | Method::HEAD) {
        return false;
    }
    let Some(if_modified_since) = request.headers.get(IF_MODIFIED_SINCE) else {
        return false;
    };
    let Some(last_modified) = response.headers.get(LAST_MODIFIED) else {
        return false;
    };
    let Ok(if_modified_since_text) = if_modified_since.to_str() else {
        return false;
    };
    let Ok(if_modified_since) = httpdate::parse_http_date(if_modified_since_text) else {
        return false;
    };
    let Ok(last_modified_text) = last_modified.to_str() else {
        return false;
    };
    let Ok(last_modified) = httpdate::parse_http_date(last_modified_text) else {
        return false;
    };
    last_modified <= if_modified_since
}

fn cached_etag_list_matches(value: &[u8], current: Option<&[u8]>) -> bool {
    value
        .split(|byte| *byte == b',')
        .map(trim_ows)
        .any(|candidate| {
            candidate == b"*" || current.is_some_and(|current| weak_etag_equal(candidate, current))
        })
}

fn weak_etag_equal(left: &[u8], right: &[u8]) -> bool {
    strip_weak_etag(left) == strip_weak_etag(right)
}

fn strip_weak_etag(value: &[u8]) -> &[u8] {
    let value = trim_ows(value);
    value
        .strip_prefix(b"W/")
        .or_else(|| value.strip_prefix(b"w/"))
        .unwrap_or(value)
}

fn trim_ows(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(start, |index| index + 1);
    &value[start..end]
}

async fn write_cached_response(
    session: &mut Session,
    response: &CachedResponse,
) -> pingora::Result<()> {
    let body_forbidden = matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let mut header = pingora::http::ResponseHeader::build(
        response.status,
        Some(response.headers.len().saturating_add(1)),
    )?;
    for (name, value) in &response.headers {
        header.append_header(name.clone(), value.clone())?;
    }
    if body_forbidden {
        header.remove_header(&CONTENT_LENGTH);
    } else {
        header.insert_header(CONTENT_LENGTH, response.body.len().to_string())?;
    }
    let head = session.req_header().method == Method::HEAD;
    let end = head || body_forbidden || response.body.is_empty();
    session.write_response_header(Box::new(header), end).await?;
    if !end {
        session
            .write_response_body(Some(response.body.clone()), true)
            .await?;
    }
    Ok(())
}

struct ConfiguredCompressionBuilder {
    gzip: Arc<HttpGzipPlan>,
}

impl HttpModuleBuilder for ConfiguredCompressionBuilder {
    fn init(&self) -> Module {
        Box::new(ConfiguredCompression {
            gzip: Arc::clone(&self.gzip),
            inner: ResponseCompressionCtx::new_for_algorithm(
                Algorithm::Gzip,
                self.gzip.level,
                false,
                false,
            )
            .with_minimum_compression_bytes(self.gzip.min_length_bytes)
            .with_content_type_filtering(false)
            .with_vary_header(self.gzip.vary),
            ready: false,
        })
    }

    fn order(&self) -> i16 {
        i16::MIN / 2
    }
}

struct ConfiguredCompression {
    gzip: Arc<HttpGzipPlan>,
    inner: ResponseCompressionCtx,
    ready: bool,
}

#[async_trait]
impl HttpModule for ConfiguredCompression {
    async fn request_header_filter(
        &mut self,
        request: &mut pingora::http::RequestHeader,
    ) -> pingora::Result<()> {
        let request_eligible = (match self.gzip.min_http_version {
            HttpGzipMinimumVersion::Http10 => request.version != http::Version::HTTP_09,
            HttpGzipMinimumVersion::Http11 => !matches!(
                request.version,
                http::Version::HTTP_09 | http::Version::HTTP_10
            ),
        }) && (!self.gzip.disable_on_via
            || !request.headers.contains_key("via"));
        if request_eligible {
            self.inner.request_filter(request);
        }
        Ok(())
    }

    async fn response_header_filter(
        &mut self,
        response: &mut pingora::http::ResponseHeader,
        end_of_stream: bool,
    ) -> pingora::Result<()> {
        if !gzip_matches(&self.gzip, response) {
            self.ready = false;
            return Ok(());
        }
        self.inner.response_header_filter(response, end_of_stream);
        self.ready = !response.status.is_informational();
        Ok(())
    }

    fn response_body_filter(
        &mut self,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
    ) -> pingora::Result<()> {
        if !self.ready {
            return Ok(());
        }
        if let Some(compressed) = self
            .inner
            .response_body_filter(body.as_ref(), end_of_stream)
        {
            *body = Some(compressed);
        }
        Ok(())
    }

    fn response_done_filter(&mut self) -> pingora::Result<Option<Bytes>> {
        if !self.ready {
            return Ok(None);
        }
        Ok(self.inner.response_body_filter(None, true))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn gzip_matches(gzip: &HttpGzipPlan, response: &pingora::http::ResponseHeader) -> bool {
    if !matches!(
        response.status,
        http::StatusCode::OK | http::StatusCode::FORBIDDEN | http::StatusCode::NOT_FOUND
    ) || response.headers.contains_key(CONTENT_ENCODING)
    {
        return false;
    }
    response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|content_type| {
            gzip.content_types
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(content_type))
        })
}

#[expect(
    clippy::too_many_lines,
    reason = "local actions and bounded internal redirects share one response state machine"
)]
async fn execute_route_action(
    service: &HttpServicePlan,
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    mut route: Arc<HttpRoutePlan>,
    method: &Method,
    mut uri: http::Uri,
) -> pingora::Result<bool> {
    const MAX_INTERNAL_REDIRECTS: usize = 10;

    let mut internal_redirects = 0;
    let mut status_override = None;
    let mut status_headers = Vec::new();
    let mut method = method.clone();
    loop {
        ctx.route = Some(Arc::clone(&route));
        match &route.action {
            HttpActionPlan::Proxy(proxy) => {
                ctx.response_status_override = status_override;
                ctx.response_header_overrides = std::mem::take(&mut status_headers);
                ctx.connection_retryable =
                    proxy.policy.max_retries > 0 && !session.is_upgrade_req();
                ctx.replay_retryable = proxy.policy.max_retries > 0
                    && (method == Method::GET || method == Method::HEAD)
                    && (session.is_body_empty() || route.policy.request_buffering)
                    && !session.is_upgrade_req();
                ctx.pool = Some(Arc::clone(&proxy.pool));
                return Ok(false);
            }
            HttpActionPlan::Fixed(response) => {
                let mut headers = response.headers.to_vec();
                headers.append(&mut status_headers);
                write_local_response(
                    session,
                    response.status,
                    &headers,
                    response.body.clone(),
                    method == Method::HEAD,
                )
                .await?;
                return Ok(true);
            }
            HttpActionPlan::Redirect(redirect) => {
                let Some(location) =
                    redirect_location(&redirect.location, session, ctx.authority.as_ref(), &uri)
                else {
                    session.respond_error(400).await?;
                    return Ok(true);
                };
                let mut headers = redirect.headers.to_vec();
                headers.append(&mut status_headers);
                headers.push((LOCATION, location));
                let body = nginx_server_marker(&headers).map_or_else(Bytes::new, |server| {
                    nginx_error_body(
                        redirect.status,
                        http::StatusCode::from_u16(redirect.status)
                            .ok()
                            .and_then(|status| status.canonical_reason())
                            .unwrap_or("Redirect"),
                        &server,
                    )
                });
                write_local_response(
                    session,
                    redirect.status,
                    &headers,
                    body,
                    method == Method::HEAD,
                )
                .await?;
                return Ok(true);
            }
            HttpActionPlan::Static(files) => {
                let mut internal_redirect: Option<(String, Vec<(HeaderName, HeaderValue)>)> = None;
                if method != Method::GET && method != Method::HEAD {
                    internal_redirect = write_static_error(
                        session,
                        files,
                        405,
                        false,
                        &[(ALLOW, HeaderValue::from_static("GET, HEAD"))],
                    )
                    .await?;
                    status_override = Some(405);
                    status_headers.clear();
                } else {
                    let result = files.serve(uri.path()).await;
                    match result {
                        Ok(StaticTarget::File(file)) => {
                            if let Some(status) = status_override.take() {
                                write_static_file_with_status(
                                    session,
                                    files,
                                    file,
                                    status,
                                    method == Method::HEAD,
                                    None,
                                    &status_headers,
                                )
                                .await?;
                                status_headers.clear();
                            } else if let Some(headers) =
                                write_static_file(session, files, file, method == Method::HEAD)
                                    .await?
                            {
                                status_headers = headers;
                                internal_redirect = write_static_error(
                                    session,
                                    files,
                                    416,
                                    method == Method::HEAD,
                                    &status_headers,
                                )
                                .await?;
                                status_override = Some(416);
                            }
                        }
                        Ok(StaticTarget::Autoindex { body }) => {
                            let status = status_override.take().unwrap_or(200);
                            let mut headers = files.headers(status);
                            headers.append(&mut status_headers);
                            headers.push((
                                CONTENT_TYPE,
                                HeaderValue::from_static("text/html; charset=utf-8"),
                            ));
                            write_local_response(
                                session,
                                status,
                                &headers,
                                body,
                                method == Method::HEAD,
                            )
                            .await?;
                        }
                        Ok(StaticTarget::Status(status)) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                status,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(status);
                            status_headers.clear();
                        }
                        Ok(StaticTarget::DirectoryRedirect { path }) => {
                            let location = internal_location(&path, &uri)?;
                            let mut headers = files.headers(301);
                            headers.push((LOCATION, location));
                            write_local_response(
                                session,
                                301,
                                &headers,
                                Bytes::new(),
                                method == Method::HEAD,
                            )
                            .await?;
                        }
                        Ok(StaticTarget::InternalRedirect { path }) => {
                            internal_redirect = Some((path, Vec::new()));
                        }
                        Err(StaticServeError::Unsafe) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                403,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(403);
                            status_headers.clear();
                        }
                        Err(StaticServeError::NotFound) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                404,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(404);
                            status_headers.clear();
                        }
                        Err(StaticServeError::TooLarge | StaticServeError::Unavailable) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                500,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(500);
                            status_headers.clear();
                        }
                    }
                }
                let Some((path, error_headers)) = internal_redirect else {
                    return Ok(true);
                };
                status_headers.extend(error_headers);
                if internal_redirects >= MAX_INTERNAL_REDIRECTS {
                    write_local_response(
                        session,
                        500,
                        &files.headers(500),
                        Bytes::new(),
                        method == Method::HEAD,
                    )
                    .await?;
                    return Ok(true);
                }
                internal_redirects += 1;
                if method != Method::GET && method != Method::HEAD {
                    method = Method::GET;
                }
                uri = internal_uri(&path, &uri)?;
                let request = session.req_header_mut();
                request.method = method.clone();
                request.uri = uri.clone();
                let Some(next) = service.select_route(ctx.authority.as_ref(), &uri, &method) else {
                    session.respond_error(404).await?;
                    return Ok(true);
                };
                if let Some(access) = &next.access
                    && !access.authorizes(&session.req_header().headers).await
                {
                    write_local_response(
                        session,
                        401,
                        &[(WWW_AUTHENTICATE, access.challenge().clone())],
                        Bytes::new(),
                        method == Method::HEAD,
                    )
                    .await?;
                    return Ok(true);
                }
                route = next;
            }
        }
    }
}

fn proxy_error_status(error: &Error) -> u16 {
    match error.etype() {
        ErrorType::HTTPStatus(code) => *code,
        _ => match error.esource() {
            ErrorSource::Upstream => 502,
            ErrorSource::Downstream => match error.etype() {
                ErrorType::WriteError | ErrorType::ReadError | ErrorType::ConnectionClosed => 0,
                _ => 400,
            },
            ErrorSource::Internal | ErrorSource::Unset => 500,
        },
    }
}

fn nginx_server_marker(headers: &[(HeaderName, HeaderValue)]) -> Option<HeaderValue> {
    let html = headers.iter().any(|(name, value)| {
        name == CONTENT_TYPE && value.as_bytes().eq_ignore_ascii_case(b"text/html")
    });
    html.then(|| {
        headers
            .iter()
            .rev()
            .find(|(name, _)| name == SERVER)
            .map(|(_, value)| value.clone())
    })
    .flatten()
}

fn nginx_error_body(status: u16, reason: &str, server: &HeaderValue) -> Bytes {
    let server = server
        .to_str()
        .expect("validated nginx server marker is visible ASCII");
    Bytes::from(format!(
        "<html>\r\n<head><title>{status} {reason}</title></head>\r\n<body>\r\n<center><h1>{status} {reason}</h1></center>\r\n<hr><center>{server}</center>\r\n</body>\r\n</html>\r\n"
    ))
}

fn internal_uri(path: &str, previous: &http::Uri) -> pingora::Result<http::Uri> {
    let path_and_query = previous
        .query()
        .map_or_else(|| path.to_owned(), |query| format!("{path}?{query}"));
    path_and_query
        .parse()
        .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

pub(crate) fn rewrite_upstream_path(
    uri: &mut http::Uri,
    rewrite: &HttpProxyPathRewrite,
) -> pingora::Result<()> {
    let Some(suffix) = uri.path().strip_prefix(&rewrite.from) else {
        return Err(Error::new_in(ErrorType::InvalidHTTPHeader));
    };
    let path = format!("{}{}", rewrite.to, suffix);
    let path_and_query = uri
        .query()
        .map_or_else(|| path.clone(), |query| format!("{path}?{query}"));
    *uri = path_and_query
        .parse()
        .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))?;
    Ok(())
}

fn internal_location(path: &str, previous: &http::Uri) -> pingora::Result<HeaderValue> {
    let uri = internal_uri(path, previous)?;
    HeaderValue::from_str(
        uri.path_and_query()
            .map_or(uri.path(), http::uri::PathAndQuery::as_str),
    )
    .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

async fn write_static_error(
    session: &mut Session,
    files: &crate::http_action::StaticFilesPlan,
    status: u16,
    head: bool,
    extra_headers: &[(HeaderName, HeaderValue)],
) -> pingora::Result<Option<(String, Vec<(HeaderName, HeaderValue)>)>> {
    if let Some(target) = files.error_document(status).await {
        match target {
            StaticErrorTarget::File { file, headers } => {
                let mut response_headers = headers.into_vec();
                response_headers.extend_from_slice(extra_headers);
                write_static_file_with_status(
                    session,
                    files,
                    file,
                    status,
                    head,
                    None,
                    &response_headers,
                )
                .await?;
                Ok(None)
            }
            StaticErrorTarget::InternalRedirect { path, headers } => {
                Ok(Some((path, headers.into_vec())))
            }
            StaticErrorTarget::Literal { body, headers } => {
                let mut response_headers = files.headers(status);
                response_headers.extend(headers);
                response_headers.extend_from_slice(extra_headers);
                write_local_response(session, status, &response_headers, body, head).await?;
                Ok(None)
            }
        }
    } else {
        let mut headers = files.headers(status);
        headers.extend_from_slice(extra_headers);
        write_local_response(session, status, &headers, Bytes::new(), head).await?;
        Ok(None)
    }
}

async fn write_static_file(
    session: &mut Session,
    files: &crate::http_action::StaticFilesPlan,
    file: StaticFile,
    head: bool,
) -> pingora::Result<Option<Vec<(HeaderName, HeaderValue)>>> {
    let validators = static_validator_headers(files, &file);
    match files.request_decision(&session.req_header().headers, &file) {
        StaticRequestDecision::NotModified => {
            let mut headers = files.headers(304);
            headers.extend(validators);
            write_local_response(session, 304, &headers, Bytes::new(), true).await?;
            return Ok(None);
        }
        StaticRequestDecision::PreconditionFailed => {
            let mut headers = files.headers(412);
            headers.extend(validators);
            write_local_response(session, 412, &headers, Bytes::new(), head).await?;
            return Ok(None);
        }
        StaticRequestDecision::RangeNotSatisfiable => {
            return Ok(Some(vec![
                (ACCEPT_RANGES, HeaderValue::from_static("bytes")),
                (
                    CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", file.size))
                        .expect("static size is a valid header value"),
                ),
            ]));
        }
        StaticRequestDecision::Serve { range } => {
            let status = if range.is_some() { 206 } else { 200 };
            write_static_file_with_status(session, files, file, status, head, range, &[]).await?;
        }
    }
    Ok(None)
}

async fn write_static_file_with_status(
    session: &mut Session,
    files: &crate::http_action::StaticFilesPlan,
    file: StaticFile,
    status: u16,
    head: bool,
    range: Option<(u64, u64)>,
    extra_headers: &[(HeaderName, HeaderValue)],
) -> pingora::Result<()> {
    let (start, end) = range.unwrap_or_else(|| (0, file.size.saturating_sub(1)));
    let length = if file.size == 0 { 0 } else { end - start + 1 };
    let mut headers = files.headers(status);
    headers.extend_from_slice(extra_headers);
    headers.extend(static_validator_headers(files, &file));
    let mut response = pingora::http::ResponseHeader::build(status, Some(headers.len() + 4))?;
    for (name, value) in headers {
        response.append_header(name, value)?;
    }
    response.insert_header(CONTENT_TYPE, files.content_type(&file.name))?;
    response.insert_header(ACCEPT_RANGES, "bytes")?;
    response.insert_header(CONTENT_LENGTH, length.to_string())?;
    if range.is_some() {
        response.insert_header(CONTENT_RANGE, format!("bytes {start}-{end}/{}", file.size))?;
    }
    if !session.is_body_empty() {
        session.set_close_on_response_before_downstream_finish(true);
    }
    session
        .write_response_header(Box::new(response), head || length == 0)
        .await?;
    if head || length == 0 {
        return Ok(());
    }

    let mut file = tokio::fs::File::from_std(file.file);
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|error| Error::because(ErrorType::ReadError, "static file seek failed", error))?;
    let mut remaining = length;
    let mut buffer = vec![0; 64 * 1024];
    while remaining > 0 {
        let chunk = usize::try_from(remaining.min(buffer.len() as u64)).expect("chunk bound");
        let read = file.read(&mut buffer[..chunk]).await.map_err(|error| {
            Error::because(ErrorType::ReadError, "static file read failed", error)
        })?;
        if read == 0 {
            return Err(Error::new_in(ErrorType::ReadError));
        }
        remaining -= u64::try_from(read).expect("read length fits u64");
        session
            .write_response_body(
                Some(Bytes::copy_from_slice(&buffer[..read])),
                remaining == 0,
            )
            .await?;
    }
    Ok(())
}

fn static_validator_headers(
    files: &crate::http_action::StaticFilesPlan,
    file: &StaticFile,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut headers = Vec::with_capacity(usize::from(files.etag_enabled()) + 1);
    if files.etag_enabled() {
        headers.push((ETAG, file.etag.clone()));
    }
    headers.push((
        LAST_MODIFIED,
        HeaderValue::from_str(&httpdate::fmt_http_date(file.modified))
            .expect("HTTP date is a valid header value"),
    ));
    headers
}

fn proxy_policy(ctx: &HttpRequestContext) -> &ProxyPolicyPlan {
    let route = proxy_route(ctx);
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        unreachable!("upstream hooks only run for proxy actions");
    };
    &proxy.policy
}

fn proxy_route(ctx: &HttpRequestContext) -> &HttpRoutePlan {
    ctx.route.as_ref().expect("proxy route context")
}

fn pingora_request_header_decisions(
    session: &Session,
    route: &HttpRoutePlan,
    authority: Option<&Authority>,
) -> Result<Box<[RequestHeaderDecision]>, RequestPolicyError> {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        return Ok(Box::new([]));
    };
    let client_ip = session
        .client_addr()
        .and_then(|address| address.as_inet())
        .map(std::net::SocketAddr::ip);
    let downstream_scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let context = RequestPolicyContext {
        authority,
        downstream_scheme,
        client_ip,
        incoming_headers: &session.req_header().headers,
    };
    let mut decisions = Vec::with_capacity(proxy.policy.request_headers.len());
    for mutation in &proxy.policy.request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        decisions.push(decide_request_header(mutation, context)?);
    }
    Ok(decisions.into_boxed_slice())
}

fn pingora_request_policy_error(error: RequestPolicyError) -> Error {
    *match error {
        RequestPolicyError::SourceTooLarge => Error::new_down(ErrorType::HTTPStatus(431)),
        RequestPolicyError::InvalidHeader => Error::new_in(ErrorType::InvalidHTTPHeader),
        RequestPolicyError::ClientIpUnavailable => {
            Error::new_in(ErrorType::Custom("ClientIpUnavailable"))
        }
        RequestPolicyError::SelectedUpstreamHostUnavailable => {
            Error::new_in(ErrorType::InternalError)
        }
    }
}

fn content_length(headers: &http::HeaderMap) -> Result<Option<u64>, ()> {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let value = match values.next() {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or(())?,
        ),
        None => None,
    };
    if values.next().is_some() {
        return Err(());
    }
    Ok(value)
}

async fn write_local_response(
    session: &mut Session,
    status: u16,
    headers: &[(HeaderName, HeaderValue)],
    body: Bytes,
    head: bool,
) -> pingora::Result<()> {
    let body_forbidden = matches!(status, 204 | 205 | 304);
    let mut response = pingora::http::ResponseHeader::build(status, Some(headers.len() + 1))?;
    for (name, value) in headers {
        response.append_header(name.clone(), value.clone())?;
    }
    if matches!(status, 204 | 304) {
        response.remove_header(&CONTENT_LENGTH);
    } else if status == 205 {
        response.insert_header(CONTENT_LENGTH, "0")?;
    } else {
        response.insert_header(CONTENT_LENGTH, body.len().to_string())?;
    }
    if !session.is_body_empty() {
        session.set_close_on_response_before_downstream_finish(true);
    }
    let end = head || body_forbidden || body.is_empty();
    session
        .write_response_header(Box::new(response), end)
        .await?;
    if !head && !body_forbidden && !body.is_empty() {
        session.write_response_body(Some(body), true).await
    } else {
        Ok(())
    }
}

fn redirect_location(
    location: &RedirectLocationPlan,
    session: &Session,
    authority: Option<&Authority>,
    uri: &http::Uri,
) -> Option<HeaderValue> {
    let scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let host = authority.map(normalized_redirect_host);
    let request_uri = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    expand_redirect_location(
        location,
        RedirectContext {
            scheme,
            normalized_host: host.as_deref(),
            request_uri,
        },
    )
}

pub(crate) fn selected_upstream_host(
    endpoint: &RuntimeEndpoint,
    policy: HttpUpstreamHost,
    incoming: Option<&Authority>,
) -> pingora::Result<Option<HeaderValue>> {
    let value = match policy {
        HttpUpstreamHost::PreserveIncoming => {
            let Some(incoming) = incoming else {
                return Ok(None);
            };
            incoming.as_str().to_owned()
        }
        HttpUpstreamHost::NginxHost { fallback } => {
            let fallback = HeaderValue::from_str(&fallback)
                .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))?;
            return Ok(nginx_request_host(incoming, &fallback)
                .map(Some)
                .map_err(pingora_request_policy_error)?);
        }
        HttpUpstreamHost::Endpoint { unix_fallback } => match endpoint {
            RuntimeEndpoint::Socket { address } => address.to_string(),
            RuntimeEndpoint::Dns { host, port } => format!("{host}:{port}"),
            RuntimeEndpoint::Unix { .. } => unix_fallback.unwrap_or_default(),
        },
        HttpUpstreamHost::Literal { value } => value,
    };
    HeaderValue::from_str(&value)
        .map(Some)
        .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

fn apply_request_header_mutations(
    request: &mut pingora::http::RequestHeader,
    ctx: &HttpRequestContext,
) -> pingora::Result<()> {
    for decision in &ctx.request_header_decisions {
        match decision {
            RequestHeaderDecision::Remove(name) => {
                request.remove_header(name);
            }
            RequestHeaderDecision::Set { name, value } => {
                let Some(value) = value
                    .complete(ctx.selected_upstream_host.as_ref())
                    .map_err(pingora_request_policy_error)?
                else {
                    continue;
                };
                request.insert_header(name.clone(), value)?;
            }
        }
    }
    Ok(())
}

fn upstream_request_requires_mutation(session: &Session, ctx: &HttpRequestContext) -> bool {
    let request = session.req_header();
    if ctx.cache_transaction.is_some() || proxy_policy(ctx).upstream_path_rewrite.is_some() {
        return true;
    }
    if request.version == http::Version::HTTP_10 {
        return true;
    }
    let host_requires_mutation = match &ctx.selected_upstream_host {
        Some(selected) => !has_single_canonical_host(request, selected),
        None => request.headers.contains_key(HOST),
    };
    if host_requires_mutation
        || ctx.pool.as_ref().is_some_and(|pool| {
            pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
        })
    {
        return true;
    }

    ctx.request_header_decisions
        .iter()
        .any(RequestHeaderDecision::requires_mutation)
}

fn has_single_canonical_host(
    request: &pingora::http::RequestHeader,
    selected: &HeaderValue,
) -> bool {
    let mut hosts = request.headers.get_all(HOST).iter();
    let Some(value) = hosts.next() else {
        return false;
    };
    if value != selected || hosts.next().is_some() {
        return false;
    }
    if !request.has_case() {
        return true;
    }
    request
        .case_header_iter()
        .find(|(name, _)| name.as_slice().eq_ignore_ascii_case(b"host"))
        .is_some_and(|(name, _)| name.as_slice() == b"Host")
}

pub(crate) fn apply_response_policy(
    response: &mut pingora::http::ResponseHeader,
    policy: &ProxyPolicyPlan,
    remove_hop_by_hop: bool,
) -> pingora::Result<()> {
    let decisions = decide_response_headers(
        response.status,
        &response.headers,
        policy,
        remove_hop_by_hop,
    )
    .map_err(|error| match error {
        ResponsePolicyError::InvalidConnectionNomination => {
            Error::new_up(ErrorType::InvalidHTTPHeader)
        }
        ResponsePolicyError::InvalidCookie => Error::new_in(ErrorType::InvalidHTTPHeader),
    })?;
    for decision in decisions {
        match decision {
            ResponseHeaderDecision::Remove(name) => {
                response.remove_header(&name);
            }
            ResponseHeaderDecision::Set { name, value } => {
                response.insert_header(name, value)?;
            }
            ResponseHeaderDecision::Add { name, value } => {
                response.append_header(name, value)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_response_policy_map(
    status: StatusCode,
    headers: &mut HeaderMap,
    policy: &ProxyPolicyPlan,
) -> pingora::Result<()> {
    let decisions =
        decide_response_headers(status, headers, policy, true).map_err(|error| match error {
            ResponsePolicyError::InvalidConnectionNomination => {
                Error::new_up(ErrorType::InvalidHTTPHeader)
            }
            ResponsePolicyError::InvalidCookie => Error::new_in(ErrorType::InvalidHTTPHeader),
        })?;
    for decision in decisions {
        match decision {
            ResponseHeaderDecision::Remove(name) => {
                headers.remove(name);
            }
            ResponseHeaderDecision::Set { name, value } => {
                headers.insert(name, value);
            }
            ResponseHeaderDecision::Add { name, value } => {
                headers.append(name, value);
            }
        }
    }
    Ok(())
}

fn connect_retry_trigger(error: &Error) -> Option<HttpRetryTrigger> {
    match error.etype() {
        ErrorType::ConnectTimedout => Some(HttpRetryTrigger::ConnectTimeout),
        ErrorType::ConnectRefused | ErrorType::ConnectNoRoute | ErrorType::ConnectError => {
            Some(HttpRetryTrigger::ConnectFailure)
        }
        _ => None,
    }
}

fn response_status_retryable(session: &Session, ctx: &HttpRequestContext) -> bool {
    if !ctx.replay_retryable
        || !request_body_replayable(session)
        || session.was_upgraded()
        || !response_is_retryable(session)
    {
        return false;
    }
    let policy = proxy_policy(ctx);
    let has_budget = ctx.attempted_upstreams.len() <= usize::from(policy.max_retries);
    let retry_target = policy.target_for_retry(ctx.attempted_upstreams.len());
    let target_available = match retry_target {
        HttpRetryTarget::SameServer => ctx.attempted_upstreams.last().is_some(),
        HttpRetryTarget::NextServer => ctx
            .pool
            .as_ref()
            .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
    };
    has_budget && target_available && ctx.remaining(true).is_ok()
}

fn response_is_retryable(session: &Session) -> bool {
    session.body_bytes_sent() == 0
        && session.response_written().is_none_or(|response| {
            response.status.is_informational() && response.status != StatusCode::SWITCHING_PROTOCOLS
        })
}

fn request_body_replayable(session: &Session) -> bool {
    session.body_bytes_read() == 0
        || (!session.retry_buffer_truncated() && session.get_retry_buffer().is_some())
}

fn retryable_upstream_error(error: &Error, session: &Session, policy: &ProxyPolicyPlan) -> bool {
    match error.etype() {
        ErrorType::HTTPStatus(status) => policy.retries_on_status(*status),
        _ => {
            response_retry_trigger(error, session).is_some_and(|trigger| policy.retries_on(trigger))
        }
    }
}

fn response_retry_trigger(error: &Error, session: &Session) -> Option<HttpRetryTrigger> {
    if is_refused_stream(error) {
        return Some(HttpRetryTrigger::RefusedStream);
    }
    match error.etype() {
        ErrorType::ConnectionClosed | ErrorType::ReadError if response_is_retryable(session) => {
            Some(HttpRetryTrigger::EmptyResponse)
        }
        ErrorType::ReadTimedout => Some(HttpRetryTrigger::ResponseTimeout),
        ErrorType::InvalidHTTPHeader
        | ErrorType::H1Error
        | ErrorType::H2Error
        | ErrorType::InvalidH2 => Some(HttpRetryTrigger::JunkResponse),
        _ => None,
    }
}

fn passive_failure_for_error(error: &Error) -> Option<HealthFailure> {
    (error.esource() == &pingora::ErrorSource::Upstream).then(|| match error.etype() {
        ErrorType::ConnectTimedout
        | ErrorType::TLSHandshakeTimedout
        | ErrorType::ReadTimedout
        | ErrorType::WriteTimedout => HealthFailure::Timeout,
        ErrorType::ConnectRefused | ErrorType::ConnectNoRoute | ErrorType::ConnectError => {
            HealthFailure::ConnectFailed
        }
        ErrorType::HTTPStatus(_) => HealthFailure::UnexpectedStatus,
        _ => HealthFailure::ProtocolError,
    })
}

const fn should_retry_connection(has_address_fallback: bool, route_retry_allowed: bool) -> bool {
    has_address_fallback || route_retry_allowed
}

fn is_refused_stream(error: &Error) -> bool {
    error
        .root_cause()
        .downcast_ref::<h2::Error>()
        .and_then(h2::Error::reason)
        == Some(h2::Reason::REFUSED_STREAM)
}

fn request_authority(request: &pingora::http::RequestHeader) -> Result<Option<Authority>, ()> {
    let uri_authority = request.uri.authority().cloned();
    let mut host_values = request.headers.get_all(HOST).iter();
    let host_authority = host_values
        .next()
        .map(|host| {
            host.to_str()
                .map_err(|_| ())?
                .parse::<Authority>()
                .map_err(|_| ())
        })
        .transpose()?;
    if host_values.next().is_some() {
        return Err(());
    }
    if uri_authority
        .iter()
        .chain(host_authority.iter())
        .any(|authority| authority.as_str().contains('@'))
    {
        return Err(());
    }

    match (uri_authority, host_authority) {
        (Some(uri), Some(host)) if uri != host => Err(()),
        (Some(uri), _) => Ok(Some(uri)),
        (None, Some(host)) => Ok(Some(host)),
        (None, None) if request.version == http::Version::HTTP_11 => Err(()),
        (None, None) => Ok(None),
    }
}

fn observe_counter(listener: &ListenerMetrics, current: usize, observed: &mut u64, received: bool) {
    let Ok(current) = u64::try_from(current) else {
        warn!("HTTP byte counter exceeds the supported range");
        return;
    };
    let Some(delta) = current.checked_sub(*observed) else {
        warn!("HTTP byte counter moved backwards");
        return;
    };
    if delta == 0 {
        return;
    }
    let result = if received {
        listener.record_bytes_received(delta)
    } else {
        listener.record_bytes_sent(delta)
    };
    match result {
        Ok(()) => *observed = current,
        Err(error) => warn!("could not account for HTTP traffic: {error}"),
    }
}

fn classify_http_result(
    error: Option<&Error>,
    response_status: Option<u16>,
) -> HttpOperationResult {
    let Some(error) = error else {
        return HttpOperationResult::from_status(response_status);
    };
    if matches!(
        error.etype(),
        ErrorType::ConnectTimedout
            | ErrorType::TLSHandshakeTimedout
            | ErrorType::ReadTimedout
            | ErrorType::WriteTimedout
    ) {
        return HttpOperationResult::Timeout;
    }
    if matches!(error.etype(), ErrorType::ConnectionClosed) {
        return HttpOperationResult::Cancelled;
    }
    if matches!(error.esource(), pingora::ErrorSource::Upstream) {
        return HttpOperationResult::UpstreamError;
    }
    if let ErrorType::HTTPStatus(status) = error.etype() {
        return HttpOperationResult::from_status(Some(*status));
    }
    HttpOperationResult::InternalError
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        net::SocketAddr,
        os::fd::AsRawFd as _,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use http::header::{SET_COOKIE, TE, UPGRADE};

    use oxiroute_config::{
        HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpProxyPathRewrite, HttpProxyPolicy,
        HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
        HttpResponseHeaderMutation, HttpRetryPolicy, HttpSameSite, UpstreamAlgorithm,
        UpstreamConnectionReuse,
    };
    use pingora::{
        protocols::{GetSocketDigest as _, SocketDigest, Stream},
        proxy::Session,
        upstreams::peer::Peer,
    };
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    #[test]
    fn upstream_path_rewrite_preserves_the_query() {
        let mut uri = "/api/items?id=7".parse().expect("request URI");
        rewrite_upstream_path(
            &mut uri,
            &HttpProxyPathRewrite {
                from: "/api/".into(),
                to: "/v1/".into(),
            },
        )
        .expect("rewritten request URI");
        assert_eq!(
            uri.path_and_query().map(http::uri::PathAndQuery::as_str),
            Some("/v1/items?id=7")
        );
    }

    #[test]
    fn nginx_gzip_status_eligibility_is_exact() {
        let gzip = HttpGzipPlan {
            level: 1,
            content_types: Box::from(["text/html".into()]),
            min_length_bytes: 20,
            min_http_version: HttpGzipMinimumVersion::Http11,
            disable_on_via: true,
            vary: true,
        };
        for (status, expected) in [
            (http::StatusCode::OK, true),
            (http::StatusCode::FORBIDDEN, true),
            (http::StatusCode::NOT_FOUND, true),
            (http::StatusCode::CREATED, false),
            (http::StatusCode::MOVED_PERMANENTLY, false),
            (http::StatusCode::INTERNAL_SERVER_ERROR, false),
            (http::StatusCode::PARTIAL_CONTENT, false),
        ] {
            let mut response = pingora::http::ResponseHeader::build(status, Some(1)).unwrap();
            response.insert_header(CONTENT_TYPE, "text/html").unwrap();
            assert_eq!(gzip_matches(&gzip, &response), expected, "{status}");
        }
        let mut encoded =
            pingora::http::ResponseHeader::build(http::StatusCode::OK, Some(2)).unwrap();
        encoded.insert_header(CONTENT_TYPE, "text/html").unwrap();
        encoded.insert_header(CONTENT_ENCODING, "br").unwrap();
        assert!(!gzip_matches(&gzip, &encoded));
    }

    #[test]
    fn http_terminal_results_use_bounded_categories() {
        use crate::HttpOperationResult;

        assert_eq!(
            classify_http_result(None, Some(204)),
            HttpOperationResult::Success
        );
        assert_eq!(
            classify_http_result(None, Some(404)),
            HttpOperationResult::ClientError
        );
        assert_eq!(
            classify_http_result(None, Some(503)),
            HttpOperationResult::ServerError
        );

        let upstream = Error::new_up(ErrorType::ConnectRefused);
        assert_eq!(
            classify_http_result(Some(&upstream), None),
            HttpOperationResult::UpstreamError
        );
        let timeout = Error::new_up(ErrorType::ReadTimedout);
        assert_eq!(
            classify_http_result(Some(&timeout), None),
            HttpOperationResult::Timeout
        );
    }

    #[test]
    fn cache_reuse_rejects_ranges_and_unsafe_preconditions() {
        let mut headers = HeaderMap::new();
        headers.insert(RANGE, HeaderValue::from_static("bytes=0-1"));
        assert!(cache_request_bypasses_cache(&headers));
        headers.remove(RANGE);
        headers.insert(IF_NONE_MATCH, HeaderValue::from_static("\"v1\""));
        assert!(!cache_request_bypasses_cache(&headers));
        headers.insert(IF_UNMODIFIED_SINCE, HeaderValue::from_static("now"));
        assert!(cache_request_bypasses_cache(&headers));
    }

    #[test]
    fn cache_etag_matching_uses_weak_comparison_and_wildcards() {
        assert!(cached_etag_list_matches(
            b"W/\"v1\", \"v2\"",
            Some(b"\"v1\"")
        ));
        assert!(cached_etag_list_matches(b"*", None));
        assert!(!cached_etag_list_matches(b"\"v1\"", Some(b"\"v2\"")));
    }

    #[test]
    fn nginx_host_preserves_ipv6_authority_brackets() {
        let authority = "[2001:db8::1]:8080".parse::<Authority>().unwrap();
        let fallback = HeaderValue::from_static("fallback.example");

        assert_eq!(
            nginx_request_host(Some(&authority), &fallback).unwrap(),
            "[2001:db8::1]"
        );
    }
    use crate::{PassiveFailurePolicy, RoundRobinPool, RouteTable, RuntimeMetrics};

    struct CloneProbe(Arc<AtomicUsize>);

    impl Clone for CloneProbe {
        fn clone(&self) -> Self {
            self.0.fetch_add(1, Ordering::Relaxed);
            Self(Arc::clone(&self.0))
        }
    }

    async fn request_preparation_fixture(
        policy: HttpProxyPolicy,
        connection_reuse: UpstreamConnectionReuse,
        request: &[u8],
    ) -> (HttpReverseProxy, Session, HttpRequestContext, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("request preparation listener");
        let client = TcpStream::connect(listener.local_addr().expect("preparation address"));
        let accept = listener.accept();
        let (client, downstream) = tokio::join!(client, accept);
        let mut client = client.expect("request preparation client");
        let (downstream, _) = downstream.expect("request preparation connection");
        client
            .write_all(request)
            .await
            .expect("write downstream request");
        let mut downstream = pingora::protocols::l4::stream::Stream::from(downstream);
        downstream.set_socket_digest(SocketDigest::from_raw_fd(downstream.as_raw_fd()));
        let mut session = Session::new_h1(Box::new(downstream));
        session
            .read_request()
            .await
            .expect("parse downstream request");

        let endpoint = RuntimeEndpoint::Socket {
            address: listener.local_addr().expect("preparation endpoint"),
        };
        let selector = Arc::new(
            RoundRobinPool::new_named(
                "preparation".into(),
                [endpoint],
                UpstreamAlgorithm::RoundRobin,
                false,
            )
            .expect("preparation selector"),
        );
        let plan = Arc::new(UpstreamPlan::with_policy(
            selector,
            None,
            None,
            None,
            connection_reuse,
        ));
        let route = Arc::new(HttpRoutePlan {
            access: None,
            action: HttpActionPlan::Proxy(crate::http_action::ProxyActionPlan {
                pool: Arc::clone(&plan),
                policy: ProxyPolicyPlan::compile(&policy),
            }),
            policy: crate::http_action::RoutePolicyPlan::compile(
                oxiroute_config::HttpRoutePolicy::default(),
            ),
            route_id: "preparation".into(),
        });
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("preparation", "http", "127.0.0.1:8080", 10)
            .expect("preparation metrics");
        let service = Arc::new(HttpServicePlan::new(
            Some(1024),
            HashMap::new(),
            Duration::from_secs(1),
            RouteTable::default(),
        ));
        let proxy = HttpReverseProxy::new(service, metrics);
        let mut context = proxy.new_ctx();
        context.authority = Some("example.test".parse().expect("request authority"));
        context.pool = Some(plan);
        context.route = Some(route);
        context.selected_upstream_host = Some(HeaderValue::from_static("example.test"));
        context.request_header_decisions = pingora_request_header_decisions(
            &session,
            context.route.as_ref().expect("request route"),
            context.authority.as_ref(),
        )
        .expect("request header decisions");
        (proxy, session, context, client)
    }

    #[cfg(unix)]
    async fn unix_request_session(request: &[u8]) -> (Session, tokio::net::UnixStream) {
        use tokio::net::{UnixListener, UnixStream};

        let directory = tempfile::tempdir().expect("Unix request directory");
        let path = directory.path().join("downstream.sock");
        let listener = UnixListener::bind(&path).expect("Unix request listener");
        let client = UnixStream::connect(&path);
        let accept = listener.accept();
        let (client, downstream) = tokio::join!(client, accept);
        let mut client = client.expect("Unix request client");
        let (downstream, _) = downstream.expect("Unix request connection");
        client
            .write_all(request)
            .await
            .expect("write Unix downstream request");
        let mut session = Session::new_h1(Box::new(downstream));
        session
            .read_request()
            .await
            .expect("parse Unix downstream request");
        (session, client)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_downstream_preserves_existing_x_forwarded_for_without_an_ip_peer() {
        let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: vec!["127.0.0.0/8".into()],
                },
            }],
            ..HttpProxyPolicy::default()
        });
        let selector = Arc::new(
            RoundRobinPool::new_named(
                "unix-xff".into(),
                [RuntimeEndpoint::Socket {
                    address: "127.0.0.1:1".parse().unwrap(),
                }],
                UpstreamAlgorithm::RoundRobin,
                false,
            )
            .unwrap(),
        );
        let route = HttpRoutePlan {
            access: None,
            action: HttpActionPlan::Proxy(crate::http_action::ProxyActionPlan {
                pool: Arc::new(UpstreamPlan::with_policy(
                    selector,
                    None,
                    None,
                    None,
                    UpstreamConnectionReuse::Safe,
                )),
                policy,
            }),
            policy: crate::http_action::RoutePolicyPlan::compile(
                oxiroute_config::HttpRoutePolicy::default(),
            ),
            route_id: "unix-xff".into(),
        };
        let (session, _client) = unix_request_session(
            b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: trusted\r\n\r\n",
        )
        .await;

        let decisions = pingora_request_header_decisions(&session, &route, None).unwrap();
        let RequestHeaderDecision::Set { value, .. } = &decisions[0] else {
            panic!("X-Forwarded-For set decision")
        };
        assert_eq!(
            value.complete(None).unwrap(),
            Some(HeaderValue::from_static("trusted"))
        );

        let (session, _client) =
            unix_request_session(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
        let decisions = pingora_request_header_decisions(&session, &route, None).unwrap();
        let RequestHeaderDecision::Set { value, .. } = &decisions[0] else {
            panic!("X-Forwarded-For no-op decision")
        };
        assert_eq!(value.complete(None).unwrap(), None);
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one request-value matrix keeps Pingora policy behavior comparable"
    )]
    async fn pingora_request_values_join_fields_and_honor_source_exceptions() {
        let request_headers = vec![
            HttpRequestHeaderMutation::Set {
                name: "x-literal".into(),
                value: HttpRequestHeaderValue::Literal {
                    value: "literal".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-authority".into(),
                value: HttpRequestHeaderValue::IncomingAuthority,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-normalized".into(),
                value: HttpRequestHeaderValue::NormalizedHost,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-nginx".into(),
                value: HttpRequestHeaderValue::NginxHost {
                    fallback: "fallback.test".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-client".into(),
                value: HttpRequestHeaderValue::ClientIp,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-scheme".into(),
                value: HttpRequestHeaderValue::DownstreamScheme,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-joined".into(),
                value: HttpRequestHeaderValue::IncomingHeader {
                    name: "x-source".into(),
                    max_bytes: 32,
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-selected".into(),
                value: HttpRequestHeaderValue::SelectedUpstreamHost,
            },
            HttpRequestHeaderMutation::Remove {
                name: "x-remove".into(),
            },
        ];
        let policy = HttpProxyPolicy {
            request_headers,
            ..HttpProxyPolicy::default()
        };
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            policy,
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: Client.Example.:8080\r\nX-Forwarded-For: trusted\r\nX-Source: one\r\nX-Source: two\r\nX-Remove: stale\r\n\r\n",
        )
        .await;
        context.authority = Some(
            "Client.Example.:8080"
                .parse()
                .expect("mixed-case authority"),
        );
        context.request_header_decisions = pingora_request_header_decisions(
            &session,
            context.route.as_ref().expect("request route"),
            context.authority.as_ref(),
        )
        .expect("request header decisions");

        let PreparedUpstreamRequest::Owned(request) = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare request values")
        else {
            panic!("request policy requires owned preparation")
        };
        for (name, expected) in [
            ("x-literal", "literal"),
            ("x-authority", "Client.Example.:8080"),
            ("x-normalized", "client.example."),
            ("x-nginx", "client.example"),
            ("x-client", "127.0.0.1"),
            ("x-forwarded-for", "trusted, 127.0.0.1"),
            ("x-scheme", "http"),
            ("x-joined", "one, two"),
            ("x-selected", "example.test"),
        ] {
            assert_eq!(
                request.headers.get(name),
                Some(&HeaderValue::from_static(expected)),
                "{name}"
            );
        }
        assert!(!request.headers.contains_key("x-remove"));

        let excepted = HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: vec!["127.0.0.0/8".into()],
                },
            }],
            ..HttpProxyPolicy::default()
        };
        let (_proxy, session, context, _client) = request_preparation_fixture(
            excepted,
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: trusted\r\n\r\n",
        )
        .await;
        let mut request = session.req_header().clone();
        apply_request_header_mutations(&mut request, &context).expect("excepted request mutation");
        assert_eq!(request.headers["x-forwarded-for"], "trusted");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pingora_join_bounds_include_inserted_separators() {
        let (mut session, _client) =
            unix_request_session(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
        let name = HeaderName::from_static("x-source");
        session
            .req_header_mut()
            .append_header(name.clone(), "abc")
            .unwrap();
        session
            .req_header_mut()
            .append_header(name.clone(), "")
            .unwrap();

        assert_eq!(
            crate::http_policy::join_header_values(&session.req_header().headers, &name, 5),
            Ok(Some(b"abc, ".to_vec()))
        );
        assert_eq!(
            crate::http_policy::join_header_values(&session.req_header().headers, &name, 4),
            Err(RequestPolicyError::SourceTooLarge)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pingora_redirect_templates_characterize_context_and_bounds() {
        let (session, _client) =
            unix_request_session(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
        let template =
            |value: &str, fallback: Option<&str>| HttpRedirectLocation::RequestTemplate {
                value: value.into(),
                nginx_host_fallback: fallback.map(str::to_owned),
            };
        let uri = "/path/to?q=1".parse().expect("redirect URI");
        for (location, authority, expected) in [
            (
                template("$scheme://$host$request_uri", None),
                Some("Normalized.Test.:8443"),
                Some("http://normalized.test/path/to?q=1"),
            ),
            (
                template("//$host$request_uri", Some("fallback.test")),
                None,
                Some("//fallback.test/path/to?q=1"),
            ),
            (template("$unknown", None), None, None),
        ] {
            let location = RedirectLocationPlan::compile(&location);
            let authority = authority.map(|value| value.parse().expect("redirect authority"));
            let actual = location.as_ref().and_then(|location| {
                redirect_location(location, &session, authority.as_ref(), &uri)
            });
            assert_eq!(
                actual.as_ref().and_then(|value| value.to_str().ok()),
                expected
            );
        }

        for (length, accepted) in [(8192, true), (8193, false)] {
            let location = HttpRedirectLocation::Literal {
                value: "x".repeat(length),
            };
            let location = RedirectLocationPlan::compile(&location);
            assert_eq!(
                location
                    .as_ref()
                    .and_then(|location| redirect_location(location, &session, None, &uri))
                    .is_some(),
                accepted,
                "literal length {length}"
            );
        }
        let boundary_uri = format!("/{}", "x".repeat(8191))
            .parse()
            .expect("boundary redirect URI");
        assert!(
            RedirectLocationPlan::compile(&template("$request_uri", None))
                .and_then(|location| {
                    redirect_location(&location, &session, None, &boundary_uri)
                })
                .is_some()
        );
        let oversized_uri = format!("/{}", "x".repeat(8192))
            .parse()
            .expect("oversized redirect URI");
        assert!(
            RedirectLocationPlan::compile(&template("$request_uri", None))
                .and_then(|location| {
                    redirect_location(&location, &session, None, &oversized_uri)
                })
                .is_none()
        );
    }

    fn response_characterization_policy() -> ProxyPolicyPlan {
        ProxyPolicyPlan::compile(&HttpProxyPolicy {
            response_headers: vec![
                HttpResponseHeaderMutation::Set {
                    name: "x-set".into(),
                    value: "set".into(),
                    always: true,
                },
                HttpResponseHeaderMutation::Add {
                    name: "x-add".into(),
                    value: "added".into(),
                    always: false,
                },
                HttpResponseHeaderMutation::Add {
                    name: "x-always".into(),
                    value: "always".into(),
                    always: true,
                },
                HttpResponseHeaderMutation::Remove {
                    name: "x-remove".into(),
                },
            ],
            response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
                from: "/internal".into(),
                to: "/".into(),
            }],
            response_cookie_attributes: vec![HttpCookieAttributePolicy {
                name: "sid".into(),
                secure: Some(false),
                http_only: Some(true),
                same_site: Some(HttpSameSite::Lax),
            }],
            ..HttpProxyPolicy::default()
        })
    }

    fn empty_proxy_policy() -> ProxyPolicyPlan {
        ProxyPolicyPlan::compile(&HttpProxyPolicy::default())
    }

    fn response_headers_fixture() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.append("x-set", HeaderValue::from_static("old-one"));
        headers.append("x-set", HeaderValue::from_static("old-two"));
        headers.append("x-add", HeaderValue::from_static("upstream"));
        headers.insert("x-remove", HeaderValue::from_static("remove"));
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static(
                "sid=1; Path=/internal; secure; HTTPONLY; SameSite=Strict; Priority=High",
            ),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("sid=2; Path=/internal"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("other=3; Path=/internal"),
        );
        headers
    }

    fn values(headers: &HeaderMap, name: impl http::header::AsHeaderName) -> Vec<&[u8]> {
        headers
            .get_all(name)
            .iter()
            .map(HeaderValue::as_bytes)
            .collect()
    }

    #[test]
    fn response_policy_adapters_match_mutation_status_and_cookie_ordering() {
        let policy = response_characterization_policy();
        for (status, expected_adds) in [(StatusCode::OK, 2), (StatusCode::NOT_FOUND, 1)] {
            let initial = response_headers_fixture();
            let mut map = initial.clone();
            apply_response_policy_map(status, &mut map, &policy).expect("HeaderMap policy");

            let mut response = pingora::http::ResponseHeader::build(status, None).unwrap();
            response.headers = initial;
            apply_response_policy(&mut response, &policy, true).expect("ResponseHeader policy");

            assert_eq!(response.headers, map);
            assert_eq!(values(&map, "x-set"), [b"set".as_slice()]);
            assert_eq!(values(&map, "x-add").len(), expected_adds);
            assert_eq!(values(&map, "x-always"), [b"always".as_slice()]);
            assert!(!map.contains_key("x-remove"));
            assert_eq!(
                values(&map, SET_COOKIE),
                [
                    b"sid=1; Path=/; HTTPONLY; SameSite=Lax; Priority=High".as_slice(),
                    b"sid=2; Path=/; HttpOnly; SameSite=Lax".as_slice(),
                    b"other=3; Path=/".as_slice(),
                ]
            );
        }
    }

    #[test]
    fn response_policy_adapters_preserve_non_utf8_cookies() {
        let policy = response_characterization_policy();
        let cookie = HeaderValue::from_bytes(b"sid=\xff; Path=/internal").unwrap();
        let mut map = HeaderMap::new();
        map.append(SET_COOKIE, cookie.clone());
        apply_response_policy_map(StatusCode::OK, &mut map, &policy).unwrap();
        assert_eq!(map[SET_COOKIE], cookie);

        let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
        response.append_header(SET_COOKIE, cookie.clone()).unwrap();
        apply_response_policy(&mut response, &policy, true).unwrap();
        assert_eq!(response.headers[SET_COOKIE], cookie);
    }

    fn hop_by_hop_fixture() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.append(CONNECTION, HeaderValue::from_static("x-first, keep-alive"));
        headers.append(CONNECTION, HeaderValue::from_static("x-second"));
        headers.insert("x-first", HeaderValue::from_static("remove"));
        headers.insert("x-second", HeaderValue::from_static("remove"));
        headers.insert(TE, HeaderValue::from_static("trailers"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert("x-end-to-end", HeaderValue::from_static("keep"));
        headers
    }

    #[test]
    fn hop_by_hop_adapters_remove_nominations_from_every_connection_field() {
        let initial = hop_by_hop_fixture();
        let mut map = initial.clone();
        apply_response_policy_map(StatusCode::OK, &mut map, &empty_proxy_policy()).unwrap();

        let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
        response.headers = initial;
        apply_response_policy(&mut response, &empty_proxy_policy(), true).unwrap();

        assert_eq!(response.headers, map);
        for name in [
            "connection",
            "x-first",
            "x-second",
            "keep-alive",
            "te",
            "upgrade",
        ] {
            assert!(!map.contains_key(name), "{name}");
        }
        assert_eq!(map["x-end-to-end"], "keep");
    }

    #[test]
    fn hop_by_hop_adapters_reject_empty_nominations_before_mutating() {
        let mut initial = HeaderMap::new();
        initial.insert(CONNECTION, HeaderValue::from_static("x-first, "));
        initial.insert("x-first", HeaderValue::from_static("still-present"));
        let mut map = initial.clone();
        assert!(
            apply_response_policy_map(StatusCode::OK, &mut map, &empty_proxy_policy()).is_err()
        );
        assert_eq!(map, initial);

        let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
        response.headers = initial.clone();
        assert!(apply_response_policy(&mut response, &empty_proxy_policy(), true).is_err());
        assert_eq!(response.headers, initial);
    }

    #[test]
    fn proxy_add_headers_append_and_honor_nginx_statuses_and_always() {
        let policy = HttpProxyPolicy {
            response_headers: vec![
                HttpResponseHeaderMutation::Add {
                    name: "x-selected".into(),
                    value: "new".into(),
                    always: false,
                },
                HttpResponseHeaderMutation::Add {
                    name: "x-always".into(),
                    value: "new".into(),
                    always: true,
                },
            ],
            ..HttpProxyPolicy::default()
        };
        let policy = ProxyPolicyPlan::compile(&policy);
        let mut error = pingora::http::ResponseHeader::build(404, None).expect("response");
        error
            .append_header("x-selected", "upstream")
            .expect("upstream header");

        apply_response_policy(&mut error, &policy, true).expect("error response policy");

        assert_eq!(error.headers.get_all("x-selected").iter().count(), 1);
        assert_eq!(error.headers.get("x-always").unwrap(), "new");

        let mut success = pingora::http::ResponseHeader::build(200, None).expect("response");
        success
            .append_header("x-selected", "upstream")
            .expect("upstream header");
        apply_response_policy(&mut success, &policy, true).expect("success response policy");
        assert_eq!(success.headers.get_all("x-selected").iter().count(), 2);
    }

    #[tokio::test]
    async fn no_case_single_host_preparation_borrows_without_cloning() {
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            HttpProxyPolicy::default(),
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
        )
        .await;
        assert!(!session.req_header().has_case());
        let hosts = session.req_header().headers.get_all(HOST);
        assert_eq!(hosts.iter().count(), 1);
        assert_eq!(
            hosts.iter().next(),
            Some(&HeaderValue::from_static("example.test"))
        );
        assert!(has_single_canonical_host(
            session.req_header(),
            context
                .selected_upstream_host
                .as_ref()
                .expect("selected Host")
        ));
        assert!(!upstream_request_requires_mutation(&session, &context));
        let clones = Arc::new(AtomicUsize::new(0));
        session
            .req_header_mut()
            .extensions
            .insert(CloneProbe(Arc::clone(&clones)));

        for _ in 0..2 {
            let prepared = proxy
                .prepare_upstream_request(&mut session, &mut context)
                .await
                .expect("prepare canonical request");
            assert!(matches!(prepared, PreparedUpstreamRequest::Borrowed));
        }

        assert_eq!(clones.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn downstream_http_1_0_is_normalized_to_the_canonical_http_1_1_upstream() {
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            HttpProxyPolicy::default(),
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.0\r\nHost: example.test\r\n\r\n",
        )
        .await;

        let PreparedUpstreamRequest::Owned(request) = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare HTTP/1.0 request")
        else {
            panic!("HTTP/1.0 requires canonical upstream normalization");
        };
        assert_eq!(request.version, http::Version::HTTP_11);
    }

    #[tokio::test]
    async fn mutating_retry_preparation_clones_once_and_starts_clean() {
        let policy = HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-policy".into(),
                value: HttpRequestHeaderValue::Literal {
                    value: "same".into(),
                },
            }],
            ..HttpProxyPolicy::default()
        };
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            policy,
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Policy: same\r\nx-policy: stale\r\n\r\n",
        )
        .await;
        let clones = Arc::new(AtomicUsize::new(0));
        session
            .req_header_mut()
            .extensions
            .insert(CloneProbe(Arc::clone(&clones)));

        let PreparedUpstreamRequest::Owned(mut first) = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare first request")
        else {
            panic!("configured Set must own the request");
        };
        assert_eq!(first.headers.get_all("x-policy").iter().count(), 1);
        first.insert_header("x-attempt", "dirty").unwrap();

        let PreparedUpstreamRequest::Owned(second) = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare retry request")
        else {
            panic!("configured Set must own the retry request");
        };
        assert_eq!(second.headers.get_all("x-policy").iter().count(), 1);
        assert!(second.headers.get("x-attempt").is_none());
        assert_eq!(
            session
                .req_header()
                .headers
                .get_all("x-policy")
                .iter()
                .count(),
            2
        );
        assert_eq!(clones.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn no_case_host_preparation_uses_semantic_value_and_duplicate_count() {
        let cases: &[(&[u8], bool)] = &[
            (b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n", false),
            (b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n", false),
            (b"GET / HTTP/1.1\r\nHOST: example.test\r\n\r\n", false),
            (b"GET / HTTP/1.1\r\nHost: other.test\r\n\r\n", true),
            (
                b"GET / HTTP/1.1\r\nHost: example.test\r\nhost: example.test\r\n\r\n",
                true,
            ),
        ];

        for (raw_request, should_own) in cases {
            let (proxy, mut session, mut context, _client) = request_preparation_fixture(
                HttpProxyPolicy::default(),
                UpstreamConnectionReuse::Safe,
                raw_request,
            )
            .await;
            let clones = Arc::new(AtomicUsize::new(0));
            session
                .req_header_mut()
                .extensions
                .insert(CloneProbe(Arc::clone(&clones)));

            let prepared = proxy
                .prepare_upstream_request(&mut session, &mut context)
                .await
                .expect("prepare Host request");
            assert_eq!(
                matches!(prepared, PreparedUpstreamRequest::Owned(_)),
                *should_own,
                "request: {}",
                String::from_utf8_lossy(raw_request)
            );
            let prepared_request = match &prepared {
                PreparedUpstreamRequest::Borrowed => session.req_header(),
                PreparedUpstreamRequest::Owned(request) => request,
            };
            let wire =
                pingora::protocols::http::v1::client::http_req_header_to_wire(prepared_request)
                    .expect("serialize prepared request");
            let wire = String::from_utf8(wire.to_vec()).expect("ASCII request wire");
            assert_eq!(wire.matches("\r\nHost: example.test\r\n").count(), 1);
            assert!(!wire.contains("\r\nhost:"));
            assert!(!wire.contains("\r\nHOST:"));
            assert_eq!(clones.load(Ordering::Relaxed), usize::from(*should_own));
        }
    }

    #[tokio::test]
    async fn no_case_duplicate_host_rejects_borrowing_and_preserves_field_order() {
        let raw_request = b"GET / HTTP/1.1\r\nX-Before: one\r\nHost: first.test\r\nhost: second.test\r\nX-After: two\r\n\r\n";
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            HttpProxyPolicy::default(),
            UpstreamConnectionReuse::Safe,
            raw_request,
        )
        .await;
        assert!(!session.req_header().has_case());
        assert_eq!(
            session
                .req_header()
                .headers
                .get_all(HOST)
                .iter()
                .map(HeaderValue::as_bytes)
                .collect::<Vec<_>>(),
            [b"first.test".as_slice(), b"second.test".as_slice()]
        );

        let PreparedUpstreamRequest::Owned(prepared) = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare duplicate Host request")
        else {
            panic!("duplicate Host fields must reject borrowed preparation");
        };
        let wire = pingora::protocols::http::v1::client::http_req_header_to_wire(&prepared)
            .expect("serialize duplicate Host preparation");
        assert_eq!(
            wire,
            &b"GET / HTTP/1.1\r\nx-before: one\r\nHost: example.test\r\nx-after: two\r\n\r\n"[..]
        );
        assert_eq!(session.req_header().headers.get_all(HOST).iter().count(), 2);
    }

    #[test]
    fn case_preserving_host_still_requires_canonical_wire_spelling() {
        let selected = HeaderValue::from_static("example.test");
        for (name, expected) in [("Host", true), ("host", false), ("HOST", false)] {
            let mut request = pingora::http::RequestHeader::build("GET", b"/", None)
                .expect("case-preserving request");
            request
                .append_header(name.to_owned(), selected.clone())
                .expect("Host header");
            assert!(request.has_case());
            assert_eq!(has_single_canonical_host(&request, &selected), expected);
        }
    }

    #[tokio::test]
    async fn connection_close_policy_promotes_once() {
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            HttpProxyPolicy::default(),
            UpstreamConnectionReuse::Never,
            b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
        )
        .await;
        let clones = Arc::new(AtomicUsize::new(0));
        session
            .req_header_mut()
            .extensions
            .insert(CloneProbe(Arc::clone(&clones)));

        let PreparedUpstreamRequest::Owned(prepared) = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare close-policy request")
        else {
            panic!("Connection close policy must own the request");
        };

        assert_eq!(prepared.headers.get(CONNECTION).unwrap(), "close");
        assert_eq!(clones.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn http_falls_back_to_the_second_address_without_route_retry_budget() {
        let listener = TcpListener::bind("127.0.0.2:0")
            .await
            .expect("second address listener");
        let second = listener.local_addr().expect("second address");
        let first = SocketAddr::from(([127, 0, 0, 1], second.port()));
        drop(
            TcpListener::bind(first)
                .await
                .expect("first address must be unused"),
        );
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: second.port(),
        };
        let selector = Arc::new(
            RoundRobinPool::new_named(
                "fallback".into(),
                [endpoint.clone()],
                UpstreamAlgorithm::RoundRobin,
                false,
            )
            .expect("fallback selector"),
        );
        let plan = Arc::new(UpstreamPlan::new(Arc::clone(&selector), None));
        let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy::default());
        assert_eq!(policy.max_retries, 0);
        let route = Arc::new(HttpRoutePlan {
            access: None,
            action: HttpActionPlan::Proxy(crate::http_action::ProxyActionPlan {
                pool: Arc::clone(&plan),
                policy,
            }),
            policy: crate::http_action::RoutePolicyPlan::compile(
                oxiroute_config::HttpRoutePolicy::default(),
            ),
            route_id: "test".into(),
        });
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("http", "http", "127.0.0.1:8080", 100)
            .expect("listener metrics");
        let service = Arc::new(HttpServicePlan::new(
            Some(1024),
            HashMap::new(),
            Duration::from_secs(1),
            RouteTable::default(),
        ));
        let proxy = HttpReverseProxy::new(service, metrics);
        let downstream_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("downstream listener");
        let client = TcpStream::connect(
            downstream_listener
                .local_addr()
                .expect("downstream address"),
        );
        let accept = downstream_listener.accept();
        let (client, downstream) = tokio::join!(client, accept);
        let _client = client.expect("downstream client");
        let (downstream, _) = downstream.expect("downstream connection");
        let mut session = Session::new_h1(Box::new(pingora::protocols::l4::stream::Stream::from(
            downstream,
        )));
        let mut context = proxy.new_ctx();
        context.attempted_upstreams.push("0".into());
        context.pool = Some(Arc::clone(&plan));
        context.route = Some(route);
        context.selected = Some(SelectedEndpoint::with_addresses(
            selector.select().expect("fallback lease"),
            vec![first, second],
        ));

        let first_peer = proxy
            .upstream_peer(&mut session, &mut context)
            .await
            .expect("first peer");
        assert_eq!(first_peer.address().as_inet(), Some(&first));
        assert!(TcpStream::connect(first).await.is_err());
        let retry = proxy.fail_to_connect(
            &mut session,
            &first_peer,
            &mut context,
            Error::new_up(ErrorType::ConnectRefused),
        );
        assert!(retry.retry());
        assert_eq!(context.attempted_upstreams.len(), 1);

        let second_peer = proxy
            .upstream_peer(&mut session, &mut context)
            .await
            .expect("second peer");
        assert_eq!(second_peer.address().as_inet(), Some(&second));
        let connection = TcpStream::connect(second)
            .await
            .expect("second address connection");
        let (_accepted, _) = listener.accept().await.expect("second address accept");
        drop(connection);
        assert_eq!(context.attempted_upstreams.len(), 1);
    }

    #[tokio::test]
    async fn passive_failures_eject_connect_and_stream_endpoints_before_retrying() {
        let policy = HttpProxyPolicy {
            retry: HttpRetryPolicy {
                max_retries: 1,
                ..HttpRetryPolicy::default()
            },
            ..HttpProxyPolicy::default()
        };
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            policy,
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
        )
        .await;
        let first_address: SocketAddr = "192.0.2.1:3000".parse().expect("first endpoint");
        let second_address: SocketAddr = "192.0.2.2:3000".parse().expect("second endpoint");
        let first = RuntimeEndpoint::Socket {
            address: first_address,
        };
        let second = RuntimeEndpoint::Socket {
            address: second_address,
        };
        let selector = Arc::new(
            RoundRobinPool::from_endpoints_with_policy(
                [first.clone(), second.clone()],
                UpstreamAlgorithm::RoundRobin,
                PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(1)),
            )
            .expect("passive selector"),
        );
        let plan = Arc::new(UpstreamPlan::with_policy(
            selector.clone(),
            None,
            None,
            None,
            UpstreamConnectionReuse::Safe,
        ));
        context.pool = Some(plan);
        context.connection_retryable = true;

        let first_peer = proxy
            .upstream_peer(&mut session, &mut context)
            .await
            .expect("first peer");
        assert_eq!(first_peer.address().as_inet(), Some(&first_address));
        let retry = proxy.fail_to_connect(
            &mut session,
            &first_peer,
            &mut context,
            Error::new_up(ErrorType::ConnectRefused),
        );
        assert!(retry.retry());
        assert!(selector.health_snapshot().endpoints[0].passive_ejected);

        let second_peer = proxy
            .upstream_peer(&mut session, &mut context)
            .await
            .expect("second peer after connect failure");
        assert_eq!(second_peer.address().as_inet(), Some(&second_address));
        let terminal = proxy.error_while_proxy(
            &second_peer,
            &mut session,
            Error::new_up(ErrorType::ReadTimedout),
            &mut context,
            false,
        );
        assert!(!terminal.retry());
        assert!(selector.health_snapshot().endpoints[1].passive_ejected);
        assert_eq!(selector.health_snapshot().available_endpoints, 0);
        assert!(selector.select().is_none());
    }

    #[test]
    fn reverse_proxy_does_not_install_disabled_compression() {
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("http", "http", "127.0.0.1:8080", 100)
            .expect("listener metrics");
        let service = Arc::new(HttpServicePlan::new(
            Some(1024),
            HashMap::new(),
            Duration::from_secs(1),
            RouteTable::default(),
        ));
        let proxy = HttpReverseProxy::new(service, metrics);
        let mut modules = HttpModules::new();

        proxy.init_downstream_modules(&mut modules);

        assert!(
            modules
                .build_ctx()
                .get::<pingora::modules::http::compression::ResponseCompression>()
                .is_none()
        );
    }

    #[tokio::test]
    async fn grpc_trailer_hooks_leave_trailers_and_body_untouched() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("trailer listener");
        let client = TcpStream::connect(listener.local_addr().expect("trailer address"));
        let accept = listener.accept();
        let (client, downstream) = tokio::join!(client, accept);
        let _client = client.expect("trailer client");
        let (downstream, _) = downstream.expect("trailer connection");
        let downstream: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(downstream));
        let mut session = Session::new_h1(downstream);
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("grpc", "http", "127.0.0.1:50051", 10)
            .expect("gRPC listener metrics");
        let service = Arc::new(HttpServicePlan::new(
            Some(1024),
            HashMap::new(),
            Duration::from_secs(1),
            RouteTable::default(),
        ));
        let proxy = HttpReverseProxy::new(service, metrics);
        let mut context = proxy.new_ctx();
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
        trailers.insert("grpc-message", http::HeaderValue::from_static("completed"));
        let expected = trailers.clone();

        proxy
            .upstream_response_trailer_filter(&mut session, &mut trailers, &mut context)
            .expect("upstream trailer hook");
        let body = proxy
            .response_trailer_filter(&mut session, &mut trailers, &mut context)
            .await
            .expect("downstream trailer hook");

        assert_eq!(trailers, expected);
        assert_eq!(body, None);
    }
}
