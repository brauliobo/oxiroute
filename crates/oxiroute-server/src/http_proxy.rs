use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use http::{
    HeaderName, HeaderValue, Method,
    header::{
        ACCEPT_RANGES, ALLOW, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HOST,
        IF_MATCH, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE, IF_UNMODIFIED_SINCE, LAST_MODIFIED,
        LOCATION, RANGE, SET_COOKIE, WWW_AUTHENTICATE,
    },
    uri::Authority,
};
use log::warn;
use oxiroute_config::{
    DownstreamTimeoutPolicy, HttpRedirectLocation, HttpRetryTarget, HttpRetryTrigger, HttpSameSite,
    HttpUpstreamHost, is_unambiguous_http_path,
};
use pingora::{
    Error, ErrorType,
    apps::{
        AcceptGate, ConnectionAdmission, HttpServerApp, HttpServerOptions, ReusedHttpStream,
        ServerApp,
    },
    modules::http::{HttpModule, HttpModuleBuilder, HttpModules, Module},
    protocols::http::compression::ResponseCompressionCtx,
    protocols::{
        ALPN, Digest, Stream,
        http::{ServerSession, v2::server::H2Options},
    },
    proxy::{PreparedUpstreamRequest, ProxyHttp, Session},
    server::ShutdownWatch,
    upstreams::peer::HttpPeer,
};

use crate::{
    GenerationReference, HttpServicePlan, ListenerMetrics, RuntimeEndpoint, RuntimeGeneration,
    RuntimeReferenceKind, TlsProfilePlan,
    http_action::{
        HttpActionPlan, HttpGzipPlan, HttpRoutePlan, ProxyPolicyPlan, RequestHeaderMutationPlan,
        RequestHeaderValuePlan, ResponseHeaderMutationPlan, StaticErrorTarget, StaticFile,
        StaticServeError, StaticTarget,
    },
    upstream_peer::{
        SelectedEndpoint, UpstreamPlan, enforce_http_version, validate_tls_connection,
    },
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

pub struct HttpReverseProxy {
    generation: Option<Arc<RuntimeGeneration>>,
    metrics: ListenerMetrics,
    service: Arc<HttpServicePlan>,
}

impl HttpReverseProxy {
    #[must_use]
    pub fn new(service: Arc<HttpServicePlan>, metrics: ListenerMetrics) -> Self {
        Self {
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
}

pub struct MonitoredHttpApp<A> {
    generation: Option<Arc<RuntimeGeneration>>,
    inner: Arc<A>,
    metrics: ListenerMetrics,
}

impl<A> MonitoredHttpApp<A> {
    #[must_use]
    pub fn new(inner: A, metrics: ListenerMetrics) -> Self {
        Self {
            generation: None,
            inner: Arc::new(inner),
            metrics,
        }
    }

    #[must_use]
    pub fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl<A> ServerApp for MonitoredHttpApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.generation.as_ref().map_or_else(
            || self.inner.accept_gate(),
            |generation| Some(generation.accept_gate()),
        )
    }

    fn accepting(&self) -> bool {
        self.metrics.accepting()
            && self.generation.as_ref().map_or_else(
                || self.inner.accepting(),
                |generation| generation.accepting(),
            )
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let generation = if let Some(generation) = &self.generation {
            let admission = generation.begin_admission()?;
            let reference = generation.begin_reference(RuntimeReferenceKind::Http1)?;
            Some((admission, reference))
        } else {
            None
        };
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected HTTP connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((generation, connection, inner)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let generation = self
            .generation
            .as_ref()
            .map(|generation| generation.begin_owned_reference(RuntimeReferenceKind::Http1));
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected HTTP connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((generation, connection, inner)))
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        self.inner.process_new(downstream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

/// Enforces listener protocol policy on a negotiated transport before HTTP parsing begins.
pub struct HttpListenerApp<A> {
    generation: Option<Arc<RuntimeGeneration>>,
    inner: Arc<A>,
    h2_only: bool,
}

impl<A> HttpListenerApp<A> {
    #[must_use]
    pub fn new(inner: A, tls_profile: Option<&TlsProfilePlan>) -> Self {
        Self {
            generation: None,
            inner: Arc::new(inner),
            h2_only: tls_profile.is_some_and(TlsProfilePlan::is_h2_only),
        }
    }

    #[must_use]
    pub fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl<A> ServerApp for HttpListenerApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.inner.accept_gate()
    }

    fn accepting(&self) -> bool {
        self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_connection()
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_owned_connection()
    }

    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let _h2_reference = if matches!(downstream.selected_alpn_proto(), Some(ALPN::H2)) {
            self.generation
                .as_ref()
                .and_then(|generation| generation.begin_reference(RuntimeReferenceKind::Http2))
        } else {
            None
        };
        if self.h2_only && !matches!(downstream.selected_alpn_proto(), Some(ALPN::H2)) {
            // A ClientHello without ALPN completes TLS, so close before Pingora's HTTP/1 fallback.
            downstream.shutdown().await;
            return None;
        }
        self.inner.process_new(downstream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

pub struct HttpDownstreamPolicyApp<A> {
    inner: Arc<A>,
    client_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    keepalive_timeout: Option<Duration>,
}

impl<A> HttpDownstreamPolicyApp<A> {
    #[must_use]
    pub fn new(inner: A, policy: DownstreamTimeoutPolicy) -> Self {
        let client_timeout = policy.client_timeout_ms.map(Duration::from_millis);
        Self {
            inner: Arc::new(inner),
            client_timeout,
            request_timeout: policy
                .request_timeout_ms
                .map(Duration::from_millis)
                .or(client_timeout),
            write_timeout: client_timeout,
            keepalive_timeout: policy.keepalive_timeout_ms.map(Duration::from_millis),
        }
    }
}

#[async_trait]
impl<A> HttpServerApp for HttpDownstreamPolicyApp<A>
where
    A: HttpServerApp + Send + Sync + 'static,
{
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        session.set_read_timeout(self.client_timeout);
        session.set_write_timeout(self.write_timeout);
        session.set_request_header_timeout(self.request_timeout);
        session.set_idle_keepalive_timeout(self.keepalive_timeout);
        if self.keepalive_timeout.is_some() {
            session.set_keepalive(Some(0));
        }
        self.inner.process_new_http(session, shutdown).await
    }

    fn h2_options(&self) -> Option<H2Options> {
        self.inner.h2_options()
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        self.inner.server_options()
    }

    async fn http_cleanup(&self) {
        self.inner.http_cleanup().await;
    }
}

pub struct HttpRequestContext {
    listener: ListenerMetrics,
    observed_received: u64,
    observed_sent: u64,
    authority: Option<Authority>,
    attempted_upstreams: Vec<String>,
    pool: Option<Arc<UpstreamPlan>>,
    route: Option<Arc<HttpRoutePlan>>,
    selected: Option<SelectedEndpoint>,
    selected_upstream_host: Option<HeaderValue>,
    connection_retryable: bool,
    replay_retryable: bool,
    retry_server: Option<String>,
    retry_delay_pending: bool,
    response_status_override: Option<u16>,
    response_header_overrides: Vec<(HeaderName, HeaderValue)>,
    started_at: Instant,
    websocket_reference: Option<GenerationReference>,
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
        HttpRequestContext {
            listener: self.metrics.clone(),
            observed_received: 0,
            observed_sent: 0,
            authority: None,
            attempted_upstreams: Vec::new(),
            pool: None,
            route: None,
            selected: None,
            selected_upstream_host: None,
            connection_retryable: false,
            replay_retryable: false,
            retry_server: None,
            retry_delay_pending: false,
            response_status_override: None,
            response_header_overrides: Vec::new(),
            started_at: Instant::now(),
            websocket_reference: None,
        }
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
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
            let Some(reference) = self
                .generation
                .as_ref()
                .and_then(|generation| generation.begin_reference(RuntimeReferenceKind::WebSocket))
            else {
                session.respond_error(503).await?;
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
            return Ok(true);
        };
        ctx.authority = authority;
        ctx.route = Some(Arc::clone(&route));
        if content_length
            .expect("checked content length")
            .is_some_and(|length| route.policy.exceeds_body_limit(length))
        {
            session.respond_error(413).await?;
            return Ok(true);
        }
        if let Some(access) = &route.access {
            if !access.authorizes(&session.req_header().headers).await {
                write_local_response(
                    session,
                    401,
                    &[(WWW_AUTHENTICATE, access.challenge().clone())],
                    Bytes::new(),
                    false,
                )
                .await?;
                return Ok(true);
            }
        }

        if !bounded_request_header_sources(session, &route)? {
            session.respond_error(431).await?;
            return Ok(true);
        }
        execute_route_action(&self.service, session, ctx, route, &method, uri).await
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let Some(pool) = &ctx.pool else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        if ctx.selected.is_none() {
            if ctx.retry_delay_pending {
                tokio::time::sleep(proxy_policy(ctx).retry_delay).await;
                ctx.retry_delay_pending = false;
            }
            let selected = if let Some(server) = ctx.retry_server.take() {
                pool.select_server_endpoint(&server)?
            } else {
                pool.select_endpoint(&ctx.attempted_upstreams)?
            };
            ctx.selected_upstream_host = selected_upstream_host(
                selected.endpoint(),
                proxy_policy(ctx).upstream_host.clone(),
                ctx.authority.as_ref(),
            )?;
            ctx.attempted_upstreams
                .push(selected.server_name().to_owned());
            ctx.selected = Some(selected);
        }
        let route_policy = proxy_route(ctx).policy;
        let peer = ctx
            .selected
            .as_mut()
            .expect("selected endpoint initialized")
            .prepare_peer_with_timeouts(
                pool,
                pool.connect_timeout(route_policy.connect_timeout),
                pool.server_timeout(route_policy.read_timeout),
                pool.server_timeout(route_policy.write_timeout),
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
            ctx.release_lease();
        }
        let policy = proxy_policy(ctx);
        let has_budget = ctx.attempted_upstreams.len() <= usize::from(policy.max_retries);
        let target_available = match policy.retry_target {
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
                && target_available,
        );
        error.set_retry(retry);
        if retry {
            if !has_address_fallback {
                if policy.retry_target == HttpRetryTarget::SameServer {
                    ctx.retry_server = ctx.attempted_upstreams.last().cloned();
                }
                ctx.retry_delay_pending = true;
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
        ctx.release_lease();
        let policy = proxy_policy(ctx);
        let has_budget = ctx.attempted_upstreams.len() <= usize::from(policy.max_retries);
        let target_available = match policy.retry_target {
            HttpRetryTarget::SameServer => ctx.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => ctx
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
        };
        let retry = ctx.replay_retryable
            && session.response_written().is_none()
            && is_refused_stream(&error)
            && policy.retries_on(HttpRetryTrigger::RefusedStream)
            && has_budget
            && target_available;
        error.set_retry(retry);
        if retry {
            if policy.retry_target == HttpRetryTarget::SameServer {
                ctx.retry_server = ctx.attempted_upstreams.last().cloned();
            }
            ctx.retry_delay_pending = true;
            ctx.listener.record_retry_attempt();
        }
        error
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

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        let result = enforce_http_version(
            ctx.pool.as_deref().and_then(UpstreamPlan::tls),
            upstream_request.version,
        );
        if result.is_err() {
            ctx.release_lease();
        }
        result?;
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
        apply_request_header_mutations(session, upstream_request, ctx)?;
        Ok(())
    }

    async fn prepare_upstream_request(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<PreparedUpstreamRequest> {
        let result = enforce_http_version(
            ctx.pool.as_deref().and_then(UpstreamPlan::tls),
            session.req_header().version,
        );
        if result.is_err() {
            ctx.release_lease();
        }
        result?;

        if !upstream_request_requires_mutation(session, ctx)? {
            return Ok(PreparedUpstreamRequest::Borrowed);
        }

        let mut upstream_request = session.req_header().clone();
        self.upstream_request_filter(session, &mut upstream_request, ctx)
            .await?;
        Ok(PreparedUpstreamRequest::Owned(Box::new(upstream_request)))
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        response: &mut pingora::http::ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        if let Some(status) = ctx.response_status_override {
            response.set_status(status)?;
        }
        for (name, value) in &ctx.response_header_overrides {
            response.append_header(name.clone(), value.clone())?;
        }
        apply_response_policy(response, proxy_policy(ctx))
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
        let result =
            validate_tls_connection(ctx.pool.as_deref().and_then(UpstreamPlan::tls), digest);
        if result.is_err() {
            ctx.release_lease();
        } else {
            // The Pingora socket digest now owns the lease until the physical connection closes.
            ctx.release_lease();
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
        ctx.observe(session);
        Ok(None)
    }

    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        if let Some(error) = error {
            warn!(
                "HTTP request failed with {:?} from {:?}",
                error.etype(),
                error.esource()
            );
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
                "host": ctx.authority.as_ref().and_then(normalized_host),
                "method": request.method.as_str(),
                "status": session.response_written().map(|response| response.status.as_u16()),
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

struct ConfiguredCompressionBuilder {
    gzip: Arc<HttpGzipPlan>,
}

impl HttpModuleBuilder for ConfiguredCompressionBuilder {
    fn init(&self) -> Module {
        Box::new(ConfiguredCompression {
            gzip: Arc::clone(&self.gzip),
            inner: ResponseCompressionCtx::new(self.gzip.level, false, false),
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
        self.inner.request_filter(request);
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
    if response.status == http::StatusCode::PARTIAL_CONTENT {
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
    loop {
        ctx.route = Some(Arc::clone(&route));
        match &route.action {
            HttpActionPlan::Proxy(proxy) => {
                ctx.response_status_override = status_override;
                ctx.response_header_overrides = std::mem::take(&mut status_headers);
                ctx.connection_retryable =
                    proxy.policy.max_retries > 0 && !session.is_upgrade_req();
                ctx.replay_retryable = proxy.policy.max_retries > 0
                    && matches!(*method, Method::GET | Method::HEAD)
                    && session.is_body_empty()
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
                    *method == Method::HEAD,
                )
                .await?;
                return Ok(true);
            }
            HttpActionPlan::Redirect(redirect) => {
                let normalized_host = ctx.authority.as_ref().and_then(normalized_host);
                let Some(location) = redirect_location(
                    &redirect.location,
                    session,
                    normalized_host.as_deref(),
                    &uri,
                ) else {
                    session.respond_error(400).await?;
                    return Ok(true);
                };
                let mut headers = redirect.headers.to_vec();
                headers.append(&mut status_headers);
                headers.push((LOCATION, location));
                write_local_response(
                    session,
                    redirect.status,
                    &headers,
                    Bytes::new(),
                    *method == Method::HEAD,
                )
                .await?;
                return Ok(true);
            }
            HttpActionPlan::Static(files) => {
                if !matches!(*method, Method::GET | Method::HEAD) {
                    let mut headers = files.headers(405);
                    headers.push((ALLOW, HeaderValue::from_static("GET, HEAD")));
                    write_local_response(session, 405, &headers, Bytes::new(), false).await?;
                    return Ok(true);
                }
                let result = files.serve(uri.path()).await;
                let mut internal_redirect = None;
                match result {
                    Ok(StaticTarget::File(file)) => {
                        if let Some(status) = status_override.take() {
                            write_static_file_with_status(
                                session,
                                files,
                                file,
                                status,
                                *method == Method::HEAD,
                                None,
                                &status_headers,
                            )
                            .await?;
                            status_headers.clear();
                        } else if let Some(headers) =
                            write_static_file(session, files, file, *method == Method::HEAD).await?
                        {
                            status_headers = headers;
                            internal_redirect = write_static_error(
                                session,
                                files,
                                416,
                                *method == Method::HEAD,
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
                            *method == Method::HEAD,
                        )
                        .await?;
                    }
                    Ok(StaticTarget::Status(status)) => {
                        internal_redirect = write_static_error(
                            session,
                            files,
                            status,
                            *method == Method::HEAD,
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
                            *method == Method::HEAD,
                        )
                        .await?;
                    }
                    Ok(StaticTarget::InternalRedirect { path }) => {
                        internal_redirect = Some(path);
                    }
                    Err(StaticServeError::Unsafe) => {
                        internal_redirect =
                            write_static_error(session, files, 403, *method == Method::HEAD, &[])
                                .await?;
                        status_override = Some(403);
                        status_headers.clear();
                    }
                    Err(StaticServeError::NotFound) => {
                        internal_redirect =
                            write_static_error(session, files, 404, *method == Method::HEAD, &[])
                                .await?;
                        status_override = Some(404);
                        status_headers.clear();
                    }
                    Err(StaticServeError::TooLarge | StaticServeError::Unavailable) => {
                        internal_redirect =
                            write_static_error(session, files, 500, *method == Method::HEAD, &[])
                                .await?;
                        status_override = Some(500);
                        status_headers.clear();
                    }
                }
                let Some(path) = internal_redirect else {
                    return Ok(true);
                };
                if internal_redirects >= MAX_INTERNAL_REDIRECTS {
                    write_local_response(
                        session,
                        500,
                        &files.headers(500),
                        Bytes::new(),
                        *method == Method::HEAD,
                    )
                    .await?;
                    return Ok(true);
                }
                internal_redirects += 1;
                uri = internal_uri(&path, &uri)?;
                session.req_header_mut().uri = uri.clone();
                let Some(next) = service.select_route(ctx.authority.as_ref(), &uri, method) else {
                    session.respond_error(404).await?;
                    return Ok(true);
                };
                if let Some(access) = &next.access {
                    if !access.authorizes(&session.req_header().headers).await {
                        write_local_response(
                            session,
                            401,
                            &[(WWW_AUTHENTICATE, access.challenge().clone())],
                            Bytes::new(),
                            *method == Method::HEAD,
                        )
                        .await?;
                        return Ok(true);
                    }
                }
                route = next;
            }
        }
    }
}

fn internal_uri(path: &str, previous: &http::Uri) -> pingora::Result<http::Uri> {
    let path_and_query = previous
        .query()
        .map_or_else(|| path.to_owned(), |query| format!("{path}?{query}"));
    path_and_query
        .parse()
        .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
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
) -> pingora::Result<Option<String>> {
    if let Some(target) = files.error_document(status).await {
        match target {
            StaticErrorTarget::File(file) => {
                write_static_file_with_status(
                    session,
                    files,
                    file,
                    status,
                    head,
                    None,
                    extra_headers,
                )
                .await?;
                Ok(None)
            }
            StaticErrorTarget::InternalRedirect(path) => Ok(Some(path)),
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
    let validators = static_validator_headers(&file);
    match static_precondition(session.req_header(), &file) {
        StaticPrecondition::NotModified => {
            let mut headers = files.headers(304);
            headers.extend(validators);
            write_local_response(session, 304, &headers, Bytes::new(), true).await?;
            return Ok(None);
        }
        StaticPrecondition::Failed => {
            let mut headers = files.headers(412);
            headers.extend(validators);
            write_local_response(session, 412, &headers, Bytes::new(), head).await?;
            return Ok(None);
        }
        StaticPrecondition::Proceed => {}
    }
    let apply_range = if_range_matches(session.req_header(), &file);
    let range = if apply_range {
        requested_range(session.req_header(), file.size)
    } else {
        Ok(None)
    };
    let Ok(range) = range else {
        return Ok(Some(vec![
            (ACCEPT_RANGES, HeaderValue::from_static("bytes")),
            (
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", file.size))
                    .expect("static size is a valid header value"),
            ),
        ]));
    };
    let status = if range.is_some() { 206 } else { 200 };
    write_static_file_with_status(session, files, file, status, head, range, &[]).await?;
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
    headers.extend(static_validator_headers(&file));
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

fn requested_range(
    request: &pingora::http::RequestHeader,
    size: u64,
) -> Result<Option<(u64, u64)>, ()> {
    let mut values = request.headers.get_all(RANGE).iter();
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
    if value.contains(',') {
        return Err(());
    }
    if size == 0 {
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

#[derive(Clone, Copy)]
enum StaticPrecondition {
    Proceed,
    NotModified,
    Failed,
}

fn static_precondition(
    request: &pingora::http::RequestHeader,
    file: &StaticFile,
) -> StaticPrecondition {
    if let Some(value) = request.headers.get(IF_MATCH) {
        let matches = value.as_bytes() == b"*" || etag_list_matches(value, &file.etag, false);
        if !matches {
            return StaticPrecondition::Failed;
        }
    } else if request
        .headers
        .get(IF_UNMODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .is_some_and(|date| modified_after(file.modified, date))
    {
        return StaticPrecondition::Failed;
    }

    if let Some(value) = request.headers.get(IF_NONE_MATCH) {
        if value.as_bytes() == b"*" || etag_list_matches(value, &file.etag, true) {
            return StaticPrecondition::NotModified;
        }
    } else if request
        .headers
        .get(IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .is_some_and(|date| !modified_after(file.modified, date))
    {
        return StaticPrecondition::NotModified;
    }
    StaticPrecondition::Proceed
}

fn if_range_matches(request: &pingora::http::RequestHeader, file: &StaticFile) -> bool {
    let Some(value) = request.headers.get(IF_RANGE) else {
        return true;
    };
    if value.as_bytes().starts_with(b"\"") {
        return value == file.etag;
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

fn static_validator_headers(file: &StaticFile) -> Vec<(HeaderName, HeaderValue)> {
    vec![
        (ETAG, file.etag.clone()),
        (
            LAST_MODIFIED,
            HeaderValue::from_str(&httpdate::fmt_http_date(file.modified))
                .expect("HTTP date is a valid header value"),
        ),
    ]
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

fn bounded_request_header_sources(
    session: &Session,
    route: &HttpRoutePlan,
) -> pingora::Result<bool> {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        return Ok(true);
    };
    for mutation in &proxy.policy.request_headers {
        let RequestHeaderMutationPlan::Set { value, .. } = mutation else {
            continue;
        };
        match value {
            RequestHeaderValuePlan::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => {
                if source_matches_exception(session, except_source_cidrs)? {
                    continue;
                }
                let client_bytes = session
                    .client_addr()
                    .and_then(|address| address.as_inet())
                    .map(|address| address.ip().to_string().len())
                    .ok_or_else(|| Error::new_in(ErrorType::Custom("ClientIpUnavailable")))?;
                let Ok(existing) = joined_header_values(
                    session.req_header(),
                    &HeaderName::from_static("x-forwarded-for"),
                    *max_bytes,
                ) else {
                    return Ok(false);
                };
                let total = existing.as_ref().map_or(0, Vec::len)
                    + usize::from(existing.is_some()) * 2
                    + client_bytes;
                if total > *max_bytes {
                    return Ok(false);
                }
            }
            RequestHeaderValuePlan::IncomingHeader { name, max_bytes } => {
                if joined_header_values(session.req_header(), name, *max_bytes).is_err() {
                    return Ok(false);
                }
            }
            RequestHeaderValuePlan::Literal(_)
            | RequestHeaderValuePlan::IncomingAuthority
            | RequestHeaderValuePlan::NormalizedHost
            | RequestHeaderValuePlan::NginxHost { .. }
            | RequestHeaderValuePlan::ClientIp
            | RequestHeaderValuePlan::DownstreamScheme
            | RequestHeaderValuePlan::SelectedUpstreamHost => {}
        }
    }
    Ok(true)
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
    location: &HttpRedirectLocation,
    session: &Session,
    host: Option<&str>,
    uri: &http::Uri,
) -> Option<HeaderValue> {
    let value = match location {
        HttpRedirectLocation::Literal { value } => value.clone(),
        HttpRedirectLocation::RequestTemplate {
            value,
            nginx_host_fallback,
        } => {
            let scheme = if session
                .digest()
                .and_then(|digest| digest.ssl_digest.as_ref())
                .is_some()
            {
                "https"
            } else {
                "http"
            };
            let request_uri = uri
                .path_and_query()
                .map_or(uri.path(), |value| value.as_str());
            let mut expanded = String::with_capacity(value.len() + request_uri.len());
            let mut remainder = value.as_str();
            while let Some((literal, variable)) = remainder.split_once('$') {
                expanded.push_str(literal);
                if let Some(after) = variable.strip_prefix("scheme") {
                    expanded.push_str(scheme);
                    remainder = after;
                } else if let Some(after) = variable.strip_prefix("host") {
                    expanded.push_str(host.or(nginx_host_fallback.as_deref()).unwrap_or_default());
                    remainder = after;
                } else if let Some(after) = variable.strip_prefix("request_uri") {
                    expanded.push_str(request_uri);
                    remainder = after;
                } else {
                    return None;
                }
            }
            expanded.push_str(remainder);
            expanded
        }
    };
    (value.len() <= 8192)
        .then(|| HeaderValue::from_str(&value).ok())
        .flatten()
}

fn normalized_host(authority: &Authority) -> Option<String> {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .map(|ip| ip.to_string())
        .ok()
        .or_else(|| Some(host.to_ascii_lowercase()))
}

fn nginx_host(
    authority: Option<&Authority>,
    fallback: &HeaderValue,
) -> pingora::Result<HeaderValue> {
    let Some(authority) = authority else {
        return Ok(fallback.clone());
    };
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let normalized = unbracketed.parse::<IpAddr>().map_or_else(
        |_| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase(),
        |ip| ip.to_string(),
    );
    dynamic_header_value(&normalized)
}

fn selected_upstream_host(
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
            return nginx_host(incoming, &fallback).map(Some);
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
    session: &Session,
    request: &mut pingora::http::RequestHeader,
    ctx: &HttpRequestContext,
) -> pingora::Result<()> {
    for mutation in &proxy_policy(ctx).request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        match mutation {
            RequestHeaderMutationPlan::Remove { name } => {
                request.remove_header(name);
            }
            RequestHeaderMutationPlan::Set { name, value } => {
                let excepted = match value {
                    RequestHeaderValuePlan::AppendedXForwardedFor {
                        except_source_cidrs,
                        ..
                    } => source_matches_exception(session, except_source_cidrs)?,
                    _ => false,
                };
                if excepted {
                    continue;
                }
                let value = match value {
                    RequestHeaderValuePlan::Literal(value) => value.clone(),
                    RequestHeaderValuePlan::IncomingAuthority => dynamic_header_value(
                        ctx.authority
                            .as_ref()
                            .map(Authority::as_str)
                            .unwrap_or_default(),
                    )?,
                    RequestHeaderValuePlan::NormalizedHost => {
                        let host = ctx
                            .authority
                            .as_ref()
                            .and_then(normalized_host)
                            .unwrap_or_default();
                        dynamic_header_value(&host)?
                    }
                    RequestHeaderValuePlan::NginxHost { fallback } => {
                        nginx_host(ctx.authority.as_ref(), fallback)?
                    }
                    RequestHeaderValuePlan::ClientIp => {
                        let client_ip = session
                            .client_addr()
                            .and_then(|address| address.as_inet())
                            .map(|address| address.ip().to_string())
                            .ok_or_else(|| {
                                Error::new_in(ErrorType::Custom("ClientIpUnavailable"))
                            })?;
                        dynamic_header_value(&client_ip)?
                    }
                    RequestHeaderValuePlan::AppendedXForwardedFor { max_bytes, .. } => {
                        appended_x_forwarded_for(session, *max_bytes)?
                    }
                    RequestHeaderValuePlan::DownstreamScheme => HeaderValue::from_static(
                        if session
                            .digest()
                            .and_then(|digest| digest.ssl_digest.as_ref())
                            .is_some()
                        {
                            "https"
                        } else {
                            "http"
                        },
                    ),
                    RequestHeaderValuePlan::IncomingHeader { name, max_bytes } => {
                        bounded_incoming_header(session.req_header(), name, *max_bytes)?
                    }
                    RequestHeaderValuePlan::SelectedUpstreamHost => ctx
                        .selected_upstream_host
                        .clone()
                        .ok_or_else(|| Error::new_in(ErrorType::InternalError))?,
                };
                request.insert_header(name.clone(), value)?;
            }
        }
    }
    Ok(())
}

fn upstream_request_requires_mutation(
    session: &Session,
    ctx: &HttpRequestContext,
) -> pingora::Result<bool> {
    let request = session.req_header();
    let host_requires_mutation = match &ctx.selected_upstream_host {
        Some(selected) => !has_single_canonical_host(request, selected),
        None => request.headers.contains_key(HOST),
    };
    if host_requires_mutation
        || ctx.pool.as_ref().is_some_and(|pool| {
            pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
        })
    {
        return Ok(true);
    }

    for mutation in &proxy_policy(ctx).request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        if let RequestHeaderMutationPlan::Set {
            value:
                RequestHeaderValuePlan::AppendedXForwardedFor {
                    except_source_cidrs,
                    ..
                },
            ..
        } = mutation
        {
            if source_matches_exception(session, except_source_cidrs)? {
                continue;
            }
        }
        return Ok(true);
    }
    Ok(false)
}

fn has_single_canonical_host(
    request: &pingora::http::RequestHeader,
    selected: &HeaderValue,
) -> bool {
    let mut hosts = request
        .case_header_iter()
        .filter(|(name, _)| name.as_slice().eq_ignore_ascii_case(b"host"));
    let Some((name, value)) = hosts.next() else {
        return false;
    };
    name.as_slice() == b"Host" && value == selected && hosts.next().is_none()
}

fn source_matches_exception(
    session: &Session,
    exceptions: &[crate::http_action::SourceCidr],
) -> pingora::Result<bool> {
    if exceptions.is_empty() {
        return Ok(false);
    }
    let client_ip = session
        .client_addr()
        .and_then(|address| address.as_inet())
        .map(std::net::SocketAddr::ip)
        .ok_or_else(|| Error::new_in(ErrorType::Custom("ClientIpUnavailable")))?;
    Ok(exceptions
        .iter()
        .any(|exception| exception.contains(client_ip)))
}

fn appended_x_forwarded_for(session: &Session, max_bytes: usize) -> pingora::Result<HeaderValue> {
    let client_ip = session
        .client_addr()
        .and_then(|address| address.as_inet())
        .map(|address| address.ip().to_string())
        .ok_or_else(|| Error::new_in(ErrorType::Custom("ClientIpUnavailable")))?;
    let existing = joined_header_values(
        session.req_header(),
        &HeaderName::from_static("x-forwarded-for"),
        max_bytes,
    )?;
    let mut value = existing.unwrap_or_default();
    if !value.is_empty() {
        value.extend_from_slice(b", ");
    }
    value.extend_from_slice(client_ip.as_bytes());
    bounded_header_value(&value, max_bytes)
}

fn bounded_incoming_header(
    request: &pingora::http::RequestHeader,
    name: &HeaderName,
    max_bytes: usize,
) -> pingora::Result<HeaderValue> {
    let value = joined_header_values(request, name, max_bytes)?.unwrap_or_default();
    bounded_header_value(&value, max_bytes)
}

fn joined_header_values(
    request: &pingora::http::RequestHeader,
    name: &HeaderName,
    max_bytes: usize,
) -> pingora::Result<Option<Vec<u8>>> {
    let mut joined = Vec::new();
    for value in &request.headers.get_all(name) {
        if !joined.is_empty() {
            joined.extend_from_slice(b", ");
        }
        if joined.len().saturating_add(value.as_bytes().len()) > max_bytes {
            return Err(Error::new_down(ErrorType::HTTPStatus(431)));
        }
        joined.extend_from_slice(value.as_bytes());
    }
    Ok((!joined.is_empty()).then_some(joined))
}

fn bounded_header_value(value: &[u8], max_bytes: usize) -> pingora::Result<HeaderValue> {
    if value.len() > max_bytes {
        return Err(Error::new_down(ErrorType::HTTPStatus(431)));
    }
    HeaderValue::from_bytes(value).map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

fn dynamic_header_value(value: &str) -> pingora::Result<HeaderValue> {
    HeaderValue::from_str(value).map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

fn apply_response_policy(
    response: &mut pingora::http::ResponseHeader,
    policy: &ProxyPolicyPlan,
) -> pingora::Result<()> {
    for mutation in &policy.response_headers {
        match mutation {
            ResponseHeaderMutationPlan::Set {
                name,
                value,
                always,
            } => {
                if *always || crate::http_action::nginx_add_header_status(response.status.as_u16())
                {
                    response.insert_header(name.clone(), value.clone())?;
                }
            }
            ResponseHeaderMutationPlan::Add {
                name,
                value,
                always,
            } => {
                if *always || crate::http_action::nginx_add_header_status(response.status.as_u16())
                {
                    response.append_header(name.clone(), value.clone())?;
                }
            }
            ResponseHeaderMutationPlan::Remove { name } => {
                response.remove_header(name);
            }
        }
    }
    if policy.cookie_path_rewrites.is_empty() && policy.cookie_attributes.is_empty() {
        return Ok(());
    }
    let cookies = response
        .headers
        .get_all(SET_COOKIE)
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    response.remove_header(&SET_COOKIE);
    for cookie in cookies {
        response.append_header(SET_COOKIE, rewrite_cookie(&cookie, policy)?)?;
    }
    Ok(())
}

fn rewrite_cookie(cookie: &HeaderValue, policy: &ProxyPolicyPlan) -> pingora::Result<HeaderValue> {
    let Ok(cookie) = cookie.to_str() else {
        return Ok(cookie.clone());
    };
    let mut segments = cookie.split(';');
    let first = segments.next().unwrap_or_default();
    let cookie_name = first
        .trim_start_matches([' ', '\t'])
        .split_once('=')
        .map(|(name, _)| name);
    let attributes = cookie_name.and_then(|name| {
        policy
            .cookie_attributes
            .iter()
            .find(|candidate| candidate.name == name)
    });
    let mut rewritten = first.to_owned();
    let mut saw_secure = false;
    let mut saw_http_only = false;
    let mut saw_same_site = false;
    for segment in segments {
        let trimmed = segment.trim_start_matches([' ', '\t']);
        let whitespace = &segment[..segment.len() - trimmed.len()];
        let (name, value) = trimmed
            .split_once('=')
            .map_or((trimmed, None), |(name, value)| (name, Some(value)));
        if name.eq_ignore_ascii_case("secure") {
            saw_secure = true;
            if attributes.and_then(|policy| policy.secure) == Some(false) {
                continue;
            }
        } else if name.eq_ignore_ascii_case("httponly") {
            saw_http_only = true;
            if attributes.and_then(|policy| policy.http_only) == Some(false) {
                continue;
            }
        } else if name.eq_ignore_ascii_case("samesite") {
            saw_same_site = true;
            if let Some(same_site) = attributes.and_then(|policy| policy.same_site) {
                append_cookie_attribute(
                    &mut rewritten,
                    whitespace,
                    "SameSite",
                    Some(same_site_value(same_site)),
                );
                continue;
            }
        }
        if let Some(value) = value {
            let replacement = (name.eq_ignore_ascii_case("path"))
                .then(|| {
                    policy
                        .cookie_path_rewrites
                        .iter()
                        .find(|rewrite| rewrite.from == value)
                })
                .flatten();
            append_cookie_attribute(
                &mut rewritten,
                whitespace,
                name,
                Some(replacement.map_or(value, |replacement| replacement.to.as_str())),
            );
        } else {
            append_cookie_attribute(&mut rewritten, whitespace, trimmed, None);
        }
    }
    if let Some(attributes) = attributes {
        if attributes.secure == Some(true) && !saw_secure {
            append_cookie_attribute(&mut rewritten, " ", "Secure", None);
        }
        if attributes.http_only == Some(true) && !saw_http_only {
            append_cookie_attribute(&mut rewritten, " ", "HttpOnly", None);
        }
        if let Some(same_site) = attributes.same_site.filter(|_| !saw_same_site) {
            append_cookie_attribute(
                &mut rewritten,
                " ",
                "SameSite",
                Some(same_site_value(same_site)),
            );
        }
    }
    HeaderValue::from_str(&rewritten).map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

fn append_cookie_attribute(output: &mut String, whitespace: &str, name: &str, value: Option<&str>) {
    output.push(';');
    output.push_str(whitespace);
    output.push_str(name);
    if let Some(value) = value {
        output.push('=');
        output.push_str(value);
    }
}

const fn same_site_value(value: HttpSameSite) -> &'static str {
    match value {
        HttpSameSite::Strict => "Strict",
        HttpSameSite::Lax => "Lax",
        HttpSameSite::None => "None",
    }
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

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use oxiroute_config::{
        HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue,
        HttpResponseHeaderMutation, UpstreamAlgorithm, UpstreamConnectionReuse,
    };
    use pingora::{
        apps::ServerApp, protocols::Stream, proxy::Session, server::ShutdownWatch,
        upstreams::peer::Peer,
    };
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::{RoundRobinPool, RouteTable, RuntimeMetrics};

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
        let mut session = Session::new_h1(Box::new(pingora::protocols::l4::stream::Stream::from(
            downstream,
        )));
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
        (proxy, session, context, client)
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

        apply_response_policy(&mut error, &policy).expect("error response policy");

        assert_eq!(error.headers.get_all("x-selected").iter().count(), 1);
        assert_eq!(error.headers.get("x-always").unwrap(), "new");

        let mut success = pingora::http::ResponseHeader::build(200, None).expect("response");
        success
            .append_header("x-selected", "upstream")
            .expect("upstream header");
        apply_response_policy(&mut success, &policy).expect("success response policy");
        assert_eq!(success.headers.get_all("x-selected").iter().count(), 2);
    }

    #[tokio::test]
    async fn canonical_h1_preserve_preparation_borrows_without_cloning() {
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            HttpProxyPolicy::default(),
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
        )
        .await;
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
    async fn host_preparation_preserves_old_canonical_wire_name_and_value_semantics() {
        let cases: &[(&[u8], bool)] = &[
            (b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n", false),
            (b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n", true),
            (b"GET / HTTP/1.1\r\nHOST: example.test\r\n\r\n", true),
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

    struct CleanupApp {
        cleaned: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ServerApp for CleanupApp {
        async fn process_new(
            self: &Arc<Self>,
            _session: Stream,
            _shutdown: &ShutdownWatch,
        ) -> Option<Stream> {
            None
        }

        async fn cleanup(&self) {
            self.cleaned.store(true, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn monitored_http_app_delegates_cleanup() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let runtime = RuntimeMetrics::new();
        let metrics = runtime
            .register_listener("http", "http", "127.0.0.1:8080", 100)
            .expect("listener metrics");
        let app = MonitoredHttpApp::new(
            CleanupApp {
                cleaned: Arc::clone(&cleaned),
            },
            metrics,
        );

        app.cleanup().await;

        assert!(cleaned.load(Ordering::Relaxed));
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
