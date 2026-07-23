use std::{net::IpAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use http::{
    HeaderName, HeaderValue, Method,
    header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, HOST, LOCATION, SET_COOKIE, WWW_AUTHENTICATE},
    uri::Authority,
};
use log::warn;
use oxiroute_config::{
    HttpRedirectLocation, HttpRetryTrigger, HttpUpstreamHost, is_unambiguous_http_path,
};
use pingora::{
    Error, ErrorType,
    apps::{ConnectionAdmission, ServerApp},
    protocols::{ALPN, Digest, Stream},
    proxy::{ProxyHttp, Session},
    server::ShutdownWatch,
    upstreams::peer::HttpPeer,
};

use crate::{
    EndpointLease, HttpServicePlan, ListenerMetrics, RuntimeEndpoint, TlsProfilePlan,
    http_action::{
        FixedResponsePlan, HttpActionPlan, HttpRoutePlan, ProxyPolicyPlan,
        RequestHeaderMutationPlan, RequestHeaderValuePlan, ResponseHeaderMutationPlan,
        StaticServeError,
    },
    upstream_peer::{UpstreamPlan, enforce_http_version, validate_tls_connection},
};

pub struct HttpReverseProxy {
    metrics: ListenerMetrics,
    service: Arc<HttpServicePlan>,
}

impl HttpReverseProxy {
    #[must_use]
    pub fn new(service: Arc<HttpServicePlan>, metrics: ListenerMetrics) -> Self {
        Self { metrics, service }
    }
}

pub struct MonitoredHttpApp<A> {
    inner: Arc<A>,
    metrics: ListenerMetrics,
}

impl<A> MonitoredHttpApp<A> {
    #[must_use]
    pub fn new(inner: A, metrics: ListenerMetrics) -> Self {
        Self {
            inner: Arc::new(inner),
            metrics,
        }
    }
}

#[async_trait]
impl<A> ServerApp for MonitoredHttpApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("rejected HTTP connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((connection, inner)))
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
    inner: Arc<A>,
    h2_only: bool,
}

impl<A> HttpListenerApp<A> {
    #[must_use]
    pub fn new(inner: A, tls_profile: Option<&TlsProfilePlan>) -> Self {
        Self {
            inner: Arc::new(inner),
            h2_only: tls_profile.is_some_and(TlsProfilePlan::is_h2_only),
        }
    }
}

#[async_trait]
impl<A> ServerApp for HttpListenerApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_connection()
    }

    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
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

pub struct HttpRequestContext {
    listener: ListenerMetrics,
    observed_received: u64,
    observed_sent: u64,
    authority: Option<Authority>,
    client_ip: Option<String>,
    normalized_host: Option<String>,
    attempted_upstreams: Vec<RuntimeEndpoint>,
    lease: Option<EndpointLease>,
    pool: Option<Arc<UpstreamPlan>>,
    route: Option<Arc<HttpRoutePlan>>,
    selected_upstream_host: Option<HeaderValue>,
    retryable: bool,
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
        self.lease.take();
    }
}

#[async_trait]
impl ProxyHttp for HttpReverseProxy {
    type CTX = HttpRequestContext;

    fn new_ctx(&self) -> Self::CTX {
        HttpRequestContext {
            listener: self.metrics.clone(),
            observed_received: 0,
            observed_sent: 0,
            authority: None,
            client_ip: None,
            normalized_host: None,
            attempted_upstreams: Vec::new(),
            lease: None,
            pool: None,
            route: None,
            selected_upstream_host: None,
            retryable: false,
        }
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        let (authority, content_length, method, uri) = {
            let request = session.req_header();
            let authority = request_authority(request);
            let content_length = content_length(&request.headers);
            (
                authority,
                content_length,
                request.method.clone(),
                request.uri.clone(),
            )
        };

        let Ok(authority) = authority else {
            session.respond_error(400).await?;
            return Ok(true);
        };
        if content_length.is_err() || !is_unambiguous_http_path(uri.path()) {
            session.respond_error(400).await?;
            return Ok(true);
        }
        if content_length
            .expect("checked content length")
            .is_some_and(|length| self.service.exceeds_body_limit(length))
        {
            session.respond_error(413).await?;
            return Ok(true);
        }

        let Some(route) = self.service.select_route(authority.as_ref(), &uri, &method) else {
            session
                .respond_error_with_body(404, Bytes::from_static(b"route not found\n"))
                .await?;
            return Ok(true);
        };
        if let Some(access) = &route.access {
            if !access.authorizes(&session.req_header().headers) {
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

        ctx.authority = authority;
        ctx.normalized_host = ctx.authority.as_ref().and_then(normalized_host);
        ctx.client_ip = session
            .client_addr()
            .and_then(|address| address.as_inet())
            .map(|address| address.ip().to_string());
        execute_route_action(session, ctx, route, &method, &uri).await
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let Some(pool) = &ctx.pool else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        let selected = pool.select_endpoint(&ctx.attempted_upstreams)?;
        ctx.attempted_upstreams.push(selected.endpoint().clone());
        ctx.selected_upstream_host = selected_upstream_host(
            selected.endpoint(),
            proxy_policy(ctx).upstream_host.clone(),
            ctx.authority.as_ref(),
        )?;
        let (peer, lease) = selected
            .prepare_peer(pool, self.service.upstream_io_timeout())
            .await?;
        ctx.lease = Some(lease);
        Ok(Box::new(peer))
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut error: Box<Error>,
    ) -> Box<Error> {
        ctx.release_lease();
        let policy = proxy_policy(ctx);
        let has_budget = ctx.attempted_upstreams.len() <= usize::from(policy.max_retries);
        let has_alternative = ctx
            .pool
            .as_ref()
            .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams));
        let trigger = connect_retry_trigger(&error);
        error.set_retry(
            ctx.retryable
                && trigger.is_some_and(|trigger| policy.retries_on(trigger))
                && has_budget
                && has_alternative,
        );
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
        let has_alternative = ctx
            .pool
            .as_ref()
            .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams));
        error.set_retry(
            ctx.retryable
                && session.response_written().is_none()
                && is_refused_stream(&error)
                && policy.retries_on(HttpRetryTrigger::RefusedStream)
                && has_budget
                && has_alternative,
        );
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
        if u64::try_from(session.body_bytes_read())
            .is_ok_and(|bytes| self.service.exceeds_body_limit(bytes))
        {
            ctx.release_lease();
            return Err(Error::new_down(ErrorType::HTTPStatus(413)));
        }
        Ok(())
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
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
        apply_request_header_mutations(upstream_request, ctx)?;
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        response: &mut pingora::http::ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
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
        _error: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        ctx.observe(session);
        ctx.release_lease();
    }
}

async fn execute_route_action(
    session: &mut Session,
    ctx: &mut HttpRequestContext,
    route: Arc<HttpRoutePlan>,
    method: &Method,
    uri: &http::Uri,
) -> pingora::Result<bool> {
    match &route.action {
        HttpActionPlan::Proxy(proxy) => {
            if !proxy.pool.has_available_endpoint() {
                session.respond_error(503).await?;
                return Ok(true);
            }
            ctx.retryable = proxy.policy.max_retries > 0
                && matches!(*method, Method::GET | Method::HEAD)
                && session.is_body_empty()
                && !session.is_upgrade_req();
            ctx.pool = Some(Arc::clone(&proxy.pool));
            ctx.route = Some(Arc::clone(&route));
            Ok(false)
        }
        HttpActionPlan::Fixed(response) => {
            write_fixed_response(session, response, *method == Method::HEAD).await?;
            Ok(true)
        }
        HttpActionPlan::Redirect(redirect) => {
            let Some(location) = redirect_location(
                &redirect.location,
                session,
                ctx.normalized_host.as_deref(),
                uri,
            ) else {
                session.respond_error(400).await?;
                return Ok(true);
            };
            write_local_response(
                session,
                redirect.status,
                &[(LOCATION, location)],
                Bytes::new(),
                *method == Method::HEAD,
            )
            .await?;
            Ok(true)
        }
        HttpActionPlan::Static(files) => {
            if !matches!(*method, Method::GET | Method::HEAD) {
                write_local_response(
                    session,
                    405,
                    &[(ALLOW, HeaderValue::from_static("GET, HEAD"))],
                    Bytes::new(),
                    false,
                )
                .await?;
                return Ok(true);
            }
            match files.serve(uri.path()).await {
                Ok(file) => {
                    write_local_response(
                        session,
                        200,
                        &[(CONTENT_TYPE, HeaderValue::from_static(file.content_type))],
                        file.body,
                        *method == Method::HEAD,
                    )
                    .await?;
                }
                Err(StaticServeError::Unsafe) => session.respond_error(403).await?,
                Err(StaticServeError::NotFound) => session.respond_error(404).await?,
                Err(StaticServeError::TooLarge | StaticServeError::Unavailable) => {
                    session.respond_error(500).await?;
                }
            }
            Ok(true)
        }
    }
}

fn proxy_policy(ctx: &HttpRequestContext) -> &ProxyPolicyPlan {
    let route = ctx.route.as_ref().expect("proxy route context");
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        unreachable!("upstream hooks only run for proxy actions");
    };
    &proxy.policy
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

async fn write_fixed_response(
    session: &mut Session,
    response: &FixedResponsePlan,
    head: bool,
) -> pingora::Result<()> {
    write_local_response(
        session,
        response.status,
        &response.headers,
        response.body.clone(),
        head,
    )
    .await
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
        HttpRedirectLocation::RequestTemplate { value } => {
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
                    expanded.push_str(host.unwrap_or_default());
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
    for mutation in &proxy_policy(ctx).request_headers {
        match mutation {
            RequestHeaderMutationPlan::Remove { name } => {
                request.remove_header(name);
            }
            RequestHeaderMutationPlan::Set { name, value } => {
                let value = match value {
                    RequestHeaderValuePlan::Literal(value) => value.clone(),
                    RequestHeaderValuePlan::IncomingAuthority => dynamic_header_value(
                        ctx.authority
                            .as_ref()
                            .map(Authority::as_str)
                            .unwrap_or_default(),
                    )?,
                    RequestHeaderValuePlan::NormalizedHost => {
                        dynamic_header_value(ctx.normalized_host.as_deref().unwrap_or_default())?
                    }
                    RequestHeaderValuePlan::ClientIp => {
                        dynamic_header_value(ctx.client_ip.as_deref().ok_or_else(|| {
                            Error::new_in(ErrorType::Custom("ClientIpUnavailable"))
                        })?)?
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

fn dynamic_header_value(value: &str) -> pingora::Result<HeaderValue> {
    HeaderValue::from_str(value).map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

fn apply_response_policy(
    response: &mut pingora::http::ResponseHeader,
    policy: &ProxyPolicyPlan,
) -> pingora::Result<()> {
    for mutation in &policy.response_headers {
        match mutation {
            ResponseHeaderMutationPlan::Set { name, value } => {
                response.insert_header(name.clone(), value.clone())?;
            }
            ResponseHeaderMutationPlan::Remove { name } => {
                response.remove_header(name);
            }
        }
    }
    if policy.cookie_path_rewrites.is_empty() {
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
        response.append_header(SET_COOKIE, rewrite_cookie_path(&cookie, policy)?)?;
    }
    Ok(())
}

fn rewrite_cookie_path(
    cookie: &HeaderValue,
    policy: &ProxyPolicyPlan,
) -> pingora::Result<HeaderValue> {
    let Ok(cookie) = cookie.to_str() else {
        return Ok(cookie.clone());
    };
    let mut segments = cookie.split(';');
    let mut rewritten = segments.next().unwrap_or_default().to_owned();
    for segment in segments {
        rewritten.push(';');
        let trimmed = segment.trim_start_matches([' ', '\t']);
        let whitespace = &segment[..segment.len() - trimmed.len()];
        rewritten.push_str(whitespace);
        let Some((name, value)) = trimmed.split_once('=') else {
            rewritten.push_str(trimmed);
            continue;
        };
        let replacement = (name.eq_ignore_ascii_case("path"))
            .then(|| {
                policy
                    .cookie_path_rewrites
                    .iter()
                    .find(|rewrite| rewrite.from == value)
            })
            .flatten();
        if let Some(replacement) = replacement {
            rewritten.push_str(name);
            rewritten.push('=');
            rewritten.push_str(&replacement.to);
        } else {
            rewritten.push_str(trimmed);
        }
    }
    HeaderValue::from_str(&rewritten).map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
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
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use pingora::{apps::ServerApp, protocols::Stream, proxy::Session, server::ShutdownWatch};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::{RouteTable, RuntimeMetrics};

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
