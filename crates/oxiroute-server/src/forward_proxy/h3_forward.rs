#[allow(clippy::wildcard_imports)]
use super::*;

impl ForwardHttp1ServicePlan {
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn handle_h3<S>(
        &self,
        request: Request<()>,
        mut stream: h3::server::RequestStream<S, Bytes>,
        client_addr: Option<SocketAddr>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        listener: crate::ListenerMetrics,
    ) where
        S: h3::quic::BidiStream<Bytes> + Send,
    {
        let AuthorizedRequest {
            approved,
            authenticated,
            parsed,
            lifetime_deadline,
            ..
        } = match self
            .authorize_request(&request, Protocol::Http3, client_addr)
            .await
        {
            Ok(request) => request,
            Err(error) => {
                let _ = send_h3_failure(&mut stream, error, self.challenge.as_ref()).await;
                return;
            }
        };

        match parsed {
            ParsedTarget::UdpTunnel(_) => {
                let _ = send_h3_failure(
                    &mut stream,
                    RequestFailure::BadRequest,
                    self.challenge.as_ref(),
                )
                .await;
            }
            ParsedTarget::Tunnel(_) => {
                let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
                let upstream = match timeout_at(
                    lifetime_deadline,
                    self.connect_tcp_until(approved.socket_addresses(), connect_deadline),
                )
                .await
                {
                    Ok(Ok(upstream)) => upstream,
                    Ok(Err(error)) => {
                        let _ = send_h3_failure(&mut stream, error, self.challenge.as_ref()).await;
                        return;
                    }
                    Err(_) => {
                        let _ = send_h3_failure(
                            &mut stream,
                            RequestFailure::GatewayTimeout,
                            self.challenge.as_ref(),
                        )
                        .await;
                        return;
                    }
                };
                let Ok(response) = Response::builder().status(StatusCode::OK).body(()) else {
                    return;
                };
                if !matches!(
                    timeout_at(lifetime_deadline, stream.send_response(response)).await,
                    Ok(Ok(()))
                ) {
                    return;
                }
                let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
                let Ok(tunnel) = BoundedTunnel::new(TunnelLimits {
                    idle_timeout: self.idle_timeout,
                    lifetime_timeout,
                    ..TunnelLimits::default()
                }) else {
                    return;
                };
                let relay = tunnel.relay_h3(stream, upstream);
                tokio::pin!(relay);
                let outcome = tokio::select! {
                    outcome = &mut relay => Some(outcome),
                    _ = shutdown.changed() => None,
                };
                match outcome {
                    Some(outcome) => {
                        log::info!(
                            target: "oxiroute::forward_proxy",
                            "event=forward_tunnel protocol=h3 destination={} outcome={} bytes_left_to_right={} bytes_right_to_left={}",
                            approved.destination().authority(),
                            outcome.kind().as_str(),
                            outcome.stats().left_to_right,
                            outcome.stats().right_to_left,
                        );
                    }
                    None => log::info!(
                        target: "oxiroute::forward_proxy",
                        "event=forward_tunnel protocol=h3 destination={} outcome=cancelled bytes_left_to_right=0 bytes_right_to_left=0",
                        approved.destination().authority(),
                    ),
                }
            }
            ParsedTarget::Forward(target) => {
                let body_limit = u64::try_from(self.max_request_body_bytes).unwrap_or(u64::MAX);
                let Ok(expected_length) = crate::http3::request_content_length(request.headers())
                else {
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadRequest,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                if expected_length.is_some_and(|length| length > body_limit) {
                    stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::PayloadTooLarge,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                let body = match recv_h3_body(
                    &mut stream,
                    self.max_request_body_bytes,
                    expected_length,
                    lifetime_deadline,
                    &mut shutdown,
                )
                .await
                {
                    Ok(body) => body,
                    Err(error) => {
                        let _ = send_h3_failure(&mut stream, error, self.challenge.as_ref()).await;
                        return;
                    }
                };
                let request_method = request.method().clone();
                if request_method.as_str().eq_ignore_ascii_case("PURGE")
                    && self
                        .cache
                        .as_ref()
                        .is_some_and(|plan| plan.purge_access.is_some())
                {
                    let response = self
                        .handle_h3_forward_cache_purge(&request, &target, body.is_empty())
                        .await;
                    let _ = send_h3_forward_response(
                        &mut stream,
                        &request_method,
                        response,
                        lifetime_deadline,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                let mut cache_transaction = match self
                    .prepare_h3_forward_cache(
                        &request,
                        &target,
                        authenticated,
                        body.is_empty(),
                        &listener,
                    )
                    .await
                {
                    H3ForwardCacheDecision::Bypass => None,
                    H3ForwardCacheDecision::Respond(response) => {
                        let response = h3_cached_forward_response(*response);
                        let _ = send_h3_forward_response(
                            &mut stream,
                            &request_method,
                            response,
                            lifetime_deadline,
                            self.challenge.as_ref(),
                        )
                        .await;
                        return;
                    }
                    H3ForwardCacheDecision::OnlyIfCached => {
                        let _ = send_h3_failure(
                            &mut stream,
                            RequestFailure::GatewayTimeout,
                            self.challenge.as_ref(),
                        )
                        .await;
                        return;
                    }
                    H3ForwardCacheDecision::Continue(transaction) => Some(*transaction),
                };
                if target.scheme == ForwardScheme::Http {
                    let response = match forward_h3_http_request(
                        self,
                        &request,
                        &target,
                        body,
                        &approved,
                        lifetime_deadline,
                        lifetime_deadline.min(Instant::now() + self.connect_timeout),
                        &mut shutdown,
                        cache_transaction.as_ref(),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(error) => {
                            send_h3_forward_failure(
                                &mut stream,
                                &request,
                                &mut cache_transaction,
                                error,
                                lifetime_deadline,
                                self.challenge.as_ref(),
                            )
                            .await;
                            return;
                        }
                    };
                    finish_h3_forward_response(
                        &mut stream,
                        &request,
                        response,
                        &mut cache_transaction,
                        lifetime_deadline,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                if target.scheme != ForwardScheme::Https {
                    if let Some(transaction) = cache_transaction.as_mut() {
                        transaction.complete_without_store();
                    }
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadGateway,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                }
                let Some(h3_upstream) = self.h3_upstream() else {
                    send_h3_forward_failure(
                        &mut stream,
                        &request,
                        &mut cache_transaction,
                        RequestFailure::BadGateway,
                        lifetime_deadline,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                let Ok(mut headers) =
                    sanitize_request_headers(request.headers(), &target.destination)
                else {
                    if let Some(transaction) = cache_transaction.as_mut() {
                        transaction.complete_without_store();
                    }
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadRequest,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                apply_header_policy(&mut headers, &self.header_policy);
                if let Some(transaction) = cache_transaction.as_ref() {
                    transaction.apply_validators(&mut headers);
                }
                headers.remove(header::HOST);
                let mut parts = request.into_parts().0;
                let path = target
                    .origin_form
                    .path_and_query()
                    .map_or(target.origin_form.path(), |value| value.as_str());
                let Ok(uri) = Uri::builder()
                    .scheme("https")
                    .authority(target.destination.authority())
                    .path_and_query(path)
                    .build()
                else {
                    if let Some(transaction) = cache_transaction.as_mut() {
                        transaction.complete_without_store();
                    }
                    let _ = send_h3_failure(
                        &mut stream,
                        RequestFailure::BadRequest,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                parts.uri = uri;
                parts.version = http::Version::HTTP_3;
                parts.headers = headers;
                if cache_transaction
                    .as_ref()
                    .is_some_and(|transaction| transaction.request().method == Method::HEAD)
                {
                    parts.method = Method::GET;
                }
                let request = Request::from_parts(parts, ());
                let server_name = match &target.destination.host {
                    Host::Dns(host) => host.as_str().to_owned(),
                    Host::Ip(address) => address.to_string(),
                };
                let mut response = None;
                let mut last_error = H3UpstreamError::Connect;
                for address in approved.socket_addresses() {
                    match h3_upstream
                        .request(
                            *address,
                            &server_name,
                            request.clone(),
                            body.clone(),
                            lifetime_deadline,
                            shutdown.clone(),
                        )
                        .await
                    {
                        Ok(value) => {
                            response = Some(value);
                            break;
                        }
                        Err(error) if error.retryable() => last_error = error,
                        Err(error) => {
                            last_error = error;
                            break;
                        }
                    }
                    if Instant::now() >= lifetime_deadline {
                        break;
                    }
                }
                let Some(response) = response else {
                    let failure = if matches!(last_error, H3UpstreamError::Timeout) {
                        RequestFailure::GatewayTimeout
                    } else {
                        RequestFailure::BadGateway
                    };
                    send_h3_forward_failure(
                        &mut stream,
                        &Request::builder()
                            .method(request_method.clone())
                            .uri(target.origin_form.clone())
                            .body(())
                            .expect("validated forward request"),
                        &mut cache_transaction,
                        failure,
                        lifetime_deadline,
                        self.challenge.as_ref(),
                    )
                    .await;
                    return;
                };
                let downstream_request = Request::builder()
                    .method(request_method)
                    .uri(target.origin_form.clone())
                    .body(())
                    .expect("validated forward request");
                finish_h3_forward_response(
                    &mut stream,
                    &downstream_request,
                    H3ForwardResponse {
                        status: response.status,
                        headers: response.headers,
                        body: response.body,
                        trailers: response.trailers,
                    },
                    &mut cache_transaction,
                    lifetime_deadline,
                    self.challenge.as_ref(),
                )
                .await;
            }
        }
    }
}

async fn recv_h3_body<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    limit: usize,
    expected_length: Option<u64>,
    deadline: Instant,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Bytes, RequestFailure>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let mut body = BytesMut::new();
    loop {
        let chunk = tokio::select! {
            result = timeout_at(deadline, stream.recv_data()) => {
                result.map_err(|_| RequestFailure::GatewayTimeout)?
                    .map_err(|_| RequestFailure::BadRequest)?
            }
            _ = shutdown.changed() => return Err(RequestFailure::GatewayTimeout),
        };
        let Some(mut chunk) = chunk else {
            if expected_length
                .is_some_and(|length| length != u64::try_from(body.len()).unwrap_or(u64::MAX))
            {
                return Err(RequestFailure::BadRequest);
            }
            return Ok(body.freeze());
        };
        if chunk.remaining() > limit.saturating_sub(body.len()) {
            stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
            return Err(RequestFailure::PayloadTooLarge);
        }
        let bytes = chunk.copy_to_bytes(chunk.remaining());
        body.extend_from_slice(&bytes);
    }
}

async fn send_h3_failure<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    failure: RequestFailure,
    challenge: Option<&HeaderValue>,
) -> Result<(), h3::error::StreamError>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let mut response = Response::new(());
    *response.status_mut() = failure.status();
    if failure == RequestFailure::Authentication
        && let Some(challenge) = challenge
    {
        response
            .headers_mut()
            .insert(header::PROXY_AUTHENTICATE, challenge.clone());
    }
    stream.send_response(response).await?;
    stream.finish().await
}

pub(super) struct H3ForwardResponse {
    status: StatusCode,
    pub(super) headers: HeaderMap,
    body: Bytes,
    trailers: Option<HeaderMap>,
}

pub(super) fn h3_forward_status_response(status: StatusCode) -> H3ForwardResponse {
    H3ForwardResponse {
        status,
        headers: HeaderMap::new(),
        body: Bytes::new(),
        trailers: None,
    }
}

fn h3_cached_forward_response(response: CachedResponse) -> H3ForwardResponse {
    let mut headers = response.headers;
    if matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
    ) {
        headers.remove(header::CONTENT_LENGTH);
    } else {
        let length = if response.status == StatusCode::RESET_CONTENT {
            0
        } else {
            response.body.len()
        };
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("bounded cache body length"),
        );
    }
    H3ForwardResponse {
        status: response.status,
        headers,
        body: response.body,
        trailers: None,
    }
}

async fn send_h3_forward_failure<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    cache_transaction: &mut Option<CacheTransaction>,
    failure: RequestFailure,
    deadline: Instant,
    challenge: Option<&HeaderValue>,
) where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let cache_class = if matches!(
        failure,
        RequestFailure::BadGateway | RequestFailure::GatewayTimeout
    ) {
        CacheFailureClass::Upstream
    } else {
        CacheFailureClass::Local
    };
    if let Some(transaction) = cache_transaction.as_mut()
        && let Some(response) = transaction.stale_response(cache_class).await
    {
        transaction.complete_without_store();
        transaction.record_hit();
        let response = h3_cached_forward_response(response);
        let _ =
            send_h3_forward_response(stream, request.method(), response, deadline, challenge).await;
        return;
    }
    if let Some(transaction) = cache_transaction.as_mut() {
        transaction.complete_without_store();
    }
    let _ = send_h3_failure(stream, failure, challenge).await;
}

#[allow(clippy::too_many_lines)]
async fn finish_h3_forward_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    mut response: H3ForwardResponse,
    cache_transaction: &mut Option<CacheTransaction>,
    deadline: Instant,
    challenge: Option<&HeaderValue>,
) where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let uncacheable_framing = response
        .trailers
        .as_ref()
        .is_some_and(|trailers| !trailers.is_empty())
        || response.headers.contains_key(header::TRANSFER_ENCODING)
        || response.headers.contains_key(header::TRAILER);
    if crate::http3::sanitize_h3_response_headers(
        &mut response.headers,
        response.status,
        u64::try_from(response.body.len()).unwrap_or(u64::MAX),
        request.method() == Method::HEAD,
    )
    .is_err()
    {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        let _ = send_h3_failure(stream, RequestFailure::BadGateway, challenge).await;
        return;
    }
    if cache_transaction
        .as_ref()
        .is_some_and(CacheTransaction::is_revalidation)
    {
        if response.status == StatusCode::NOT_MODIFIED {
            let transaction = cache_transaction
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
            let response = h3_cached_forward_response(cached);
            let _ =
                send_h3_forward_response(stream, request.method(), response, deadline, challenge)
                    .await;
            return;
        }
        if response.status.is_server_error()
            && let Some(transaction) = cache_transaction.as_mut()
            && let Some(stale) = transaction
                .stale_response(CacheFailureClass::Upstream)
                .await
        {
            transaction.complete_without_store();
            transaction.record_hit();
            let response = h3_cached_forward_response(stale);
            let _ =
                send_h3_forward_response(stream, request.method(), response, deadline, challenge)
                    .await;
            return;
        }
    }

    let mut prepared = None;
    if let Some(transaction) = cache_transaction.as_mut() {
        if transaction.request().method == Method::GET && !uncacheable_framing {
            let tags = transaction
                .surrogate_header()
                .map_or_else(Vec::new, |header| {
                    response_surrogate_tags_forward(&response.headers, header)
                });
            if transaction.cache_tags_within_limits(&tags)
                && response_representation_valid_forward(
                    response.status,
                    &response.headers,
                    response.body.len(),
                )
            {
                let tag_refs = tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
                let timing = ResponseTiming {
                    request_started: transaction.request().request_started,
                    response_received: transaction.now(),
                    response_received_wall: SystemTime::now(),
                };
                prepared = transaction
                    .prepare_response(
                        response.status,
                        &response.headers,
                        response.body.clone(),
                        timing,
                        &tag_refs,
                    )
                    .ok();
            }
        }
        if prepared.is_none() {
            transaction.complete_without_store();
        }
    }

    let sent =
        send_h3_forward_response(stream, request.method(), response, deadline, challenge).await;
    if !sent {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return;
    }
    if let Some(entry) = prepared
        && let Some(transaction) = cache_transaction.as_mut()
    {
        let _ = transaction.admit(entry).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward_h3_http_request(
    service: &ForwardHttp1ServicePlan,
    request: &Request<()>,
    target: &oxiroute_forward_proxy::ForwardTarget,
    body: Bytes,
    approved: &ApprovedDestination,
    lifetime_deadline: Instant,
    connect_deadline: Instant,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    cache_transaction: Option<&CacheTransaction>,
) -> Result<H3ForwardResponse, RequestFailure> {
    let ConnectedHttp {
        stream: upstream,
        via_peer,
    } = tokio::select! {
        result = timeout_at(
            lifetime_deadline,
            service.connect_http_with_peers(
                &target.destination,
                ForwardScheme::Http,
                approved.socket_addresses(),
                connect_deadline,
            ),
        ) => match result {
            Ok(Ok(upstream)) => upstream,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        },
        _ = shutdown.changed() => return Err(RequestFailure::GatewayTimeout),
    };
    let request_uri = if via_peer {
        absolute_form(target).ok_or(RequestFailure::BadRequest)?
    } else {
        target.origin_form.clone()
    };
    let mut headers = sanitize_request_headers(request.headers(), &target.destination)
        .map_err(|_| RequestFailure::BadRequest)?;
    apply_header_policy(&mut headers, &service.header_policy);
    if let Some(transaction) = cache_transaction {
        transaction.apply_validators(&mut headers);
    }
    headers.remove(header::CONTENT_LENGTH);
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&body.len().to_string()).expect("bounded H3 request body length"),
    );
    let mut parts = request.clone().into_parts().0;
    if cache_transaction.is_some_and(|transaction| transaction.request().method == Method::HEAD) {
        parts.method = Method::GET;
    }
    parts.uri = request_uri;
    parts.version = http::Version::HTTP_11;
    parts.headers = headers;
    let request = Request::from_parts(parts, Full::new(body));
    let (mut sender, connection) = tokio::select! {
        result = timeout_at(
            lifetime_deadline,
            hyper::client::conn::http1::handshake(TokioIo::new(upstream)),
        ) => match result {
            Ok(Ok(connection)) => connection,
            Ok(Err(_)) => return Err(RequestFailure::BadGateway),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        },
        _ = shutdown.changed() => return Err(RequestFailure::GatewayTimeout),
    };
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let response = tokio::select! {
        result = timeout_at(lifetime_deadline, sender.send_request(request)) => match result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(RequestFailure::BadGateway),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        },
        _ = shutdown.changed() => return Err(RequestFailure::GatewayTimeout),
    };
    let status = response.status();
    let headers = response.headers().clone();
    let body = TimedBody::new(
        response.into_body(),
        service.idle_timeout,
        lifetime_deadline,
    );
    let collected = tokio::select! {
        result = timeout_at(
            lifetime_deadline,
            Limited::new(
                body,
                usize::try_from(crate::http3::H3_MAX_RESPONSE_BODY_BYTES)
                    .expect("H3 response limit fits usize"),
            )
            .collect(),
        ) => match result {
            Ok(Ok(collected)) => collected,
            Ok(Err(_)) => return Err(RequestFailure::BadGateway),
            Err(_) => return Err(RequestFailure::GatewayTimeout),
        },
        _ = shutdown.changed() => return Err(RequestFailure::GatewayTimeout),
    };
    let trailers = collected.trailers().cloned();
    Ok(H3ForwardResponse {
        status,
        headers,
        body: collected.to_bytes(),
        trailers,
    })
}

async fn send_h3_forward_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    method: &Method,
    mut response: H3ForwardResponse,
    deadline: Instant,
    challenge: Option<&HeaderValue>,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let head = *method == Method::HEAD;
    if crate::http3::sanitize_h3_response_headers(
        &mut response.headers,
        response.status,
        u64::try_from(response.body.len()).unwrap_or(u64::MAX),
        head,
    )
    .is_err()
    {
        let _ = send_h3_failure(stream, RequestFailure::BadGateway, challenge).await;
        return false;
    }
    let body_forbidden = matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let mut head_response = Response::new(());
    *head_response.status_mut() = response.status;
    *head_response.version_mut() = http::Version::HTTP_3;
    *head_response.headers_mut() = response.headers;
    if !matches!(
        timeout_at(deadline, stream.send_response(head_response)).await,
        Ok(Ok(()))
    ) {
        return false;
    }
    if !head
        && !body_forbidden
        && !response.body.is_empty()
        && !matches!(
            timeout_at(deadline, stream.send_data(response.body)).await,
            Ok(Ok(()))
        )
    {
        return false;
    }
    if !head && let Some(mut trailers) = response.trailers {
        if crate::http3::sanitize_h3_trailers(&mut trailers).is_err() {
            return false;
        }
        if !matches!(
            timeout_at(deadline, stream.send_trailers(trailers)).await,
            Ok(Ok(()))
        ) {
            return false;
        }
    }
    matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(())))
}
