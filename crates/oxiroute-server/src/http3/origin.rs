//! The upstream transports of a reverse HTTP/3 route. An exact HTTP/3 pool is reached with the QUIC client; every
//! other pool is an HTTP/1.1 or HTTP/2 origin, reached with the connector the classic path uses, so connections are
//! pooled, secured and bounded by the pool's own policy. Either way the answer comes back whole, with the status,
//! the fields and the trailers the origin sent, for the attempt loop to retry, cache and send.

use http::{header::CONNECTION, uri::PathAndQuery};
use oxiroute_config::UpstreamConnectionReuse;
use pingora::{
    ErrorType,
    connectors::{ConnectorOptions, http::Connector},
    http::RequestHeader,
    protocols::http::client::HttpSession,
    upstreams::peer::{HttpPeer, Peer as _},
};

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::{
    H3UpstreamPlan,
    http::action::RoutePolicyPlan,
    http3::upstream_plan::H3UpstreamResponse,
    upstream_peer::{
        SelectedEndpoint, UpstreamPlan, enforce_http_version, validate_tls_connection,
    },
};

/// Idle origin connections one listener keeps for reuse.
const ORIGIN_IDLE_CONNECTIONS: usize = 1024;

/// An origin response whose declared length is above this, or that declares none, is sent to the client as it arrives
/// instead of being read whole first. Smaller ones are buffered: they leave in one write and the attempt loop may still
/// retry, cache and measure them.
pub(super) const H3_STREAM_FROM: u64 = 256 * 1024;

/// The connections of a listener to HTTP/1.1 and HTTP/2 origins.
pub(super) struct OriginClient {
    connector: Connector,
}

impl OriginClient {
    pub(super) fn new() -> Self {
        let options = ConnectorOptions::new(ORIGIN_IDLE_CONNECTIONS);
        Self {
            connector: Connector::new(Some(options)),
        }
    }
}

/// What an attempt on an upstream brings back.
pub(super) enum Answer<'a> {
    /// The head, the body and the trailers, all read.
    Whole(Box<H3UpstreamResponse>),
    /// The head of an answer whose body is still with the origin and goes on to the client as it arrives.
    Open(Box<OpenAnswer<'a>>),
}

impl Answer<'_> {
    pub(super) fn status(&self) -> StatusCode {
        match self {
            Self::Whole(response) => response.status,
            Self::Open(open) => open.status,
        }
    }
}

/// An origin connection with the head of the answer read; its body is read with [`OpenAnswer::chunk`].
pub(super) struct OpenAnswer<'a> {
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap,
    session: HttpSession,
    peer: HttpPeer,
    client: &'a OriginClient,
}

impl OpenAnswer<'_> {
    /// The next piece of the body, `None` once it ended.
    pub(super) async fn chunk(&mut self) -> Result<Option<Bytes>, H3UpstreamError> {
        Ok(self.session.read_response_body().await?)
    }

    pub(super) async fn trailers(&mut self) -> Result<Option<HeaderMap>, H3UpstreamError> {
        Ok(read_trailers(&mut self.session).await?)
    }

    /// Gives the connection back for reuse; call it only after the body ended.
    pub(super) async fn release(self) {
        let idle = self.peer.idle_timeout();
        self.client
            .connector
            .release_http_session(self.session, &self.peer, idle)
            .await;
    }
}

pub(super) enum Upstream<'a> {
    Quic {
        plan: &'a H3UpstreamPlan,
        server_name: &'a str,
    },
    Origin(Origin<'a>),
}

pub(super) struct Origin<'a> {
    client: &'a OriginClient,
    pool: &'a UpstreamPlan,
    route: RoutePolicyPlan,
    timeouts: oxiroute_config::HttpProxyTimeouts,
}

impl<'a> Upstream<'a> {
    /// The transport of a proxy route; the message says why the route cannot be served.
    pub(super) fn select(
        proxy: &'a ProxyActionPlan,
        route: RoutePolicyPlan,
        client: &'a OriginClient,
    ) -> Result<Self, &'static [u8]> {
        let Some(plan) = proxy.pool.h3() else {
            return Ok(Self::Origin(Origin {
                client,
                pool: &proxy.pool,
                route,
                timeouts: proxy.policy.timeouts,
            }));
        };
        let server_name = plan
            .server_name()
            .ok_or(b"HTTP/3 upstream SNI is unavailable\n".as_slice())?;
        Ok(Self::Quic { plan, server_name })
    }

    pub(super) fn build_request(
        &self,
        policy: &ProxyPolicyPlan,
        request: &Request<()>,
        body_length: usize,
        selected_host: &HeaderValue,
        decisions: &[RequestHeaderDecision],
    ) -> Result<Request<()>, ()> {
        match self {
            Self::Quic { .. } => {
                build_h3_upstream_request(request, body_length, selected_host, policy, decisions)
            }
            Self::Origin(origin) => {
                origin.build_request(policy, request, body_length, selected_host, decisions)
            }
        }
    }

    /// One attempt on the selected endpoint, with `remaining` of the request's time left.
    pub(super) async fn exchange(
        &self,
        selected: &mut SelectedEndpoint,
        request: &Request<()>,
        body: &Bytes,
        (remaining, deadline): (Duration, Instant),
        shutdown: &watch::Receiver<bool>,
        may_stream: bool,
    ) -> Result<Answer<'_>, H3UpstreamError> {
        match self {
            Self::Quic { plan, server_name } => {
                let address = selected
                    .prepare_h3_address(remaining)
                    .await
                    .map_err(|_| H3UpstreamError::Connect)?;
                let (request, body) = (request.clone(), body.clone());
                plan.request(
                    address,
                    server_name,
                    request,
                    body,
                    deadline,
                    shutdown.clone(),
                )
                .await
                .map(|response| Answer::Whole(Box::new(response)))
            }
            Self::Origin(origin) => {
                let mut shutdown = shutdown.clone();
                tokio::select! {
                    changed = shutdown.changed() => {
                        let _ = changed;
                        Err(H3UpstreamError::Cancelled)
                    }
                    result = timeout_at(deadline, origin.round_trip(selected, request, body, remaining, may_stream)) => {
                        result.unwrap_or(Err(H3UpstreamError::Timeout))
                    }
                }
            }
        }
    }
}

impl<'a> Origin<'a> {
    /// The origin-form request an origin gets: the route's `Host`, the length of a buffered body, no `Content-Length`
    /// that the client did not send for an empty one, and a `Connection: close` from a pool that never reuses
    /// connections.
    fn build_request(
        &self,
        policy: &ProxyPolicyPlan,
        request: &Request<()>,
        body_length: usize,
        selected_host: &HeaderValue,
        decisions: &[RequestHeaderDecision],
    ) -> Result<Request<()>, ()> {
        let close = self.pool.connection_reuse() == UpstreamConnectionReuse::Never;
        let headers = upstream_headers(request, selected_host, decisions, |headers| {
            headers.insert(HOST, selected_host.clone());
            if body_length > 0 {
                headers.insert(CONTENT_LENGTH, HeaderValue::from(body_length));
            }
            if close {
                headers.insert(CONNECTION, HeaderValue::from_static("close"));
            }
        })?;
        let mut output = Request::builder()
            .method(request.method().clone())
            .uri(upstream_path(request, policy)?)
            .body(())
            .map_err(|_| ())?;
        *output.headers_mut() = headers;
        Ok(output)
    }

    async fn round_trip(
        &self,
        selected: &mut SelectedEndpoint,
        request: &Request<()>,
        body: &Bytes,
        remaining: Duration,
        may_stream: bool,
    ) -> Result<Answer<'a>, H3UpstreamError> {
        let peer = self.peer(selected, remaining).await?;
        let connector = &self.client.connector;
        let (mut session, _reused) = connector.get_http_session(&peer).await?;
        // The connection is the lease's from here: a reused one never took capacity, so the wait ends now.
        selected.end_queue_wait();
        self.admit(&session)?;
        apply_timeouts(&mut session, &peer);
        let (status, headers) = send_request(&mut session, request, body).await?;
        if may_stream && streams(status, &headers)? {
            return Ok(Answer::Open(Box::new(OpenAnswer {
                status,
                headers,
                session,
                peer,
                client: self.client,
            })));
        }
        let response = read_answer(&mut session, status, headers).await?;
        connector
            .release_http_session(session, &peer, peer.idle_timeout())
            .await;
        Ok(Answer::Whole(Box::new(response)))
    }

    /// The peer of the attempt, with the timeouts the classic path gives it.
    async fn peer(
        &self,
        selected: &mut SelectedEndpoint,
        remaining: Duration,
    ) -> Result<HttpPeer, H3UpstreamError> {
        let timeouts = self.timeouts;
        let attempt = |explicit: Option<u64>, inherited: Duration| {
            explicit
                .map_or(inherited, Duration::from_millis)
                .min(remaining)
        };
        let response = self.pool.server_timeout(self.route.read_timeout);
        let mut peer = selected
            .prepare_peer_with_timeouts(
                self.pool,
                std::time::Instant::now(),
                attempt(
                    timeouts.connect_ms,
                    self.pool.connect_timeout(self.route.connect_timeout),
                ),
                attempt(timeouts.header_ms, response),
                self.pool
                    .server_timeout(self.route.write_timeout)
                    .min(remaining),
            )
            .await?;
        peer.options.body_read_timeout = Some(attempt(timeouts.idle_ms, response));
        Ok(peer)
    }

    /// What the classic path demands of a connection once it exists: the TLS the pool names and its HTTP version.
    fn admit(&self, session: &HttpSession) -> Result<(), H3UpstreamError> {
        let version = if matches!(session, HttpSession::H2(_)) {
            http::Version::HTTP_2
        } else {
            http::Version::HTTP_11
        };
        validate_tls_connection(self.pool.tls(), session.digest())
            .and_then(|()| enforce_http_version(self.pool.tls(), version))
            .map_err(|_| H3UpstreamError::Failed)
    }
}

fn apply_timeouts(session: &mut HttpSession, peer: &HttpPeer) {
    let options = &peer.options;
    match session {
        HttpSession::H1(h1) => {
            h1.read_timeout = options.read_timeout;
            h1.body_read_timeout = options.body_read_timeout;
            h1.write_timeout = options.write_timeout;
        }
        HttpSession::H2(h2) => {
            h2.read_timeout = options.read_timeout;
            h2.body_read_timeout = options.body_read_timeout;
            h2.write_timeout = options.write_timeout;
        }
        HttpSession::Custom(()) => {}
    }
}

/// Sends the request and reads the head of the final answer.
async fn send_request(
    session: &mut HttpSession,
    request: &Request<()>,
    body: &Bytes,
) -> Result<(StatusCode, HeaderMap), H3UpstreamError> {
    let head = origin_request_header(request, matches!(session, HttpSession::H2(_)))?;
    session.write_request_header(head).await?;
    if !body.is_empty() {
        session.write_request_body(body.clone(), true).await?;
    }
    session.finish_request_body().await?;
    read_head(session).await
}

/// Whether the body of an answer goes to the client as it arrives: it has no declared length or a long one.
fn streams(status: StatusCode, headers: &HeaderMap) -> Result<bool, H3UpstreamError> {
    if body_forbidden(status) {
        return Ok(false);
    }
    let declared = request_content_length(headers).map_err(|()| H3UpstreamError::JunkResponse)?;
    Ok(declared.is_none_or(|length| length > H3_STREAM_FROM))
}

/// The body and the trailers of an answer whose head was read.
async fn read_answer(
    session: &mut HttpSession,
    status: StatusCode,
    headers: HeaderMap,
) -> Result<H3UpstreamResponse, H3UpstreamError> {
    let body = read_body(session, &headers).await?;
    let trailers = read_trailers(session).await?;
    Ok(H3UpstreamResponse {
        status,
        headers,
        body,
        trailers,
    })
}

async fn read_trailers(session: &mut HttpSession) -> pingora::Result<Option<HeaderMap>> {
    match session {
        HttpSession::H2(h2) => h2.read_trailers().await,
        HttpSession::H1(_) | HttpSession::Custom(()) => Ok(None),
    }
}

fn origin_request_header(
    request: &Request<()>,
    h2: bool,
) -> Result<Box<RequestHeader>, H3UpstreamError> {
    let path = request
        .uri()
        .path_and_query()
        .map_or(request.uri().path(), PathAndQuery::as_str);
    let mut head = RequestHeader::build(
        request.method().clone(),
        path.as_bytes(),
        Some(request.headers().len()),
    )?;
    for (name, value) in request.headers() {
        if h2 && name == CONNECTION {
            continue;
        }
        head.append_header(name, value.clone())?;
    }
    Ok(Box::new(head))
}

/// The final response head; informational ones before it are the origin's own business, an upgrade is refused.
async fn read_head(session: &mut HttpSession) -> Result<(StatusCode, HeaderMap), H3UpstreamError> {
    loop {
        session.read_response_header().await?;
        let head = session
            .response_header()
            .ok_or(H3UpstreamError::JunkResponse)?;
        if head.status == StatusCode::SWITCHING_PROTOCOLS {
            return Err(H3UpstreamError::JunkResponse);
        }
        if !head.status.is_informational() {
            return Ok((head.status, head.headers.clone()));
        }
    }
}

async fn read_body(
    session: &mut HttpSession,
    headers: &HeaderMap,
) -> Result<Bytes, H3UpstreamError> {
    let declared = request_content_length(headers).map_err(|()| H3UpstreamError::JunkResponse)?;
    if declared.is_some_and(|length| length > H3_MAX_RESPONSE_BODY_BYTES) {
        return Err(H3UpstreamError::ResponseBodyTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = session.read_response_body().await? {
        let length = u64::try_from(body.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
        if length > H3_MAX_RESPONSE_BODY_BYTES {
            return Err(H3UpstreamError::ResponseBodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

/// The classic path's classification of a failure of the connection or the exchange.
impl From<Box<pingora::Error>> for H3UpstreamError {
    fn from(error: Box<pingora::Error>) -> Self {
        log::debug!("reverse HTTP/3 origin exchange failed: {error}");
        let refused = error
            .root_cause()
            .downcast_ref::<h2::Error>()
            .and_then(h2::Error::reason);
        if refused == Some(h2::Reason::REFUSED_STREAM) {
            return Self::RefusedStream;
        }
        match error.etype() {
            ErrorType::ConnectTimedout | ErrorType::TLSHandshakeTimedout => Self::Timeout,
            ErrorType::ReadTimedout | ErrorType::WriteTimedout => Self::ResponseTimeout,
            ErrorType::ConnectRefused
            | ErrorType::ConnectNoRoute
            | ErrorType::ConnectError
            | ErrorType::ConnectProxyFailure
            | ErrorType::TLSHandshakeFailure
            | ErrorType::HandshakeError
            | ErrorType::InvalidCert => Self::Connect,
            ErrorType::HTTPStatus(_) => Self::QueueTimeout,
            ErrorType::ConnectionClosed | ErrorType::ReadError => Self::EmptyResponse,
            ErrorType::InvalidHTTPHeader
            | ErrorType::H1Error
            | ErrorType::H2Error
            | ErrorType::InvalidH2 => Self::JunkResponse,
            _ => Self::Failed,
        }
    }
}
