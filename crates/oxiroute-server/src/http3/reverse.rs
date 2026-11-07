#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_reverse_request<C>(
    resolver: RequestResolver<C, Bytes>,
    service: Arc<HttpServicePlan>,
    metrics: ListenerMetrics,
    client_addr: Option<SocketAddr>,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
    request_slots: Arc<Semaphore>,
) where
    C: h3::quic::Connection<Bytes> + Send + 'static,
    C::BidiStream: h3::quic::BidiStream<Bytes> + Send + 'static,
{
    let Ok((request, mut stream)) = resolver.resolve_request().await else {
        return;
    };
    let Ok(_request_slot) = request_slots.try_acquire_owned() else {
        let deadline = Instant::now() + Duration::from_secs(1);
        let _ = send_h3_response(
            &mut stream,
            StatusCode::SERVICE_UNAVAILABLE,
            &[],
            Bytes::new(),
            false,
            deadline,
        )
        .await;
        return;
    };

    let started_at = Instant::now();
    let deadline = request_deadline(&service, downstream_timeouts, started_at);
    let method = request.method().clone();
    let uri = request.uri().clone();
    let authority = request_authority(&request);
    let response_status = if let Some(authority) = authority.as_ref() {
        if header_bytes(request.headers())
            > usize::try_from(H3_MAX_FIELD_SECTION_BYTES).expect("H3 field limit fits usize")
        {
            send_h3_error(
                &mut stream,
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                b"request headers exceed the HTTP/3 limit\n",
                deadline,
            )
            .await
            .then_some(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.as_u16())
        } else if !is_unambiguous_http_path(uri.path()) {
            send_h3_error(
                &mut stream,
                StatusCode::BAD_REQUEST,
                b"request path is invalid\n",
                deadline,
            )
            .await
            .then_some(StatusCode::BAD_REQUEST.as_u16())
        } else {
            let dispatch_shutdown = shutdown.clone();
            tokio::select! {
                _ = shutdown.changed() => None,
                response = dispatch_reverse_request(
                    &mut stream,
                    &request,
                    authority,
                    &service,
                    &metrics,
                    client_addr,
                    deadline,
                    dispatch_shutdown,
                ) => response,
            }
        }
    } else {
        send_h3_error(
            &mut stream,
            StatusCode::BAD_REQUEST,
            b"request authority is required\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_REQUEST.as_u16())
    };

    let result = response_status.map_or(HttpOperationResult::Cancelled, |status| {
        HttpOperationResult::from_status(Some(status))
    });
    if let Some(access_log) = service.access_log()
        && let Err(error) = access_log.write(&serde_json::json!({
            "timestampUnixMs": unix_time_ms(),
            "service": access_log.service(),
            "protocol": "h3",
            "host": authority.as_ref().map(Authority::host),
            "method": method.as_str(),
            "status": response_status,
            "clientIp": client_addr.map(|address| address.ip().to_string()),
            "durationMs": started_at.elapsed().as_millis().to_string(),
        }))
    {
        warn!("reverse HTTP/3 access log write failed: {error}");
    }
    if let Err(error) = metrics.record_http_operation(result, started_at.elapsed()) {
        warn!("reverse HTTP/3 operation metrics failed: {error}");
    }
}

#[allow(
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::too_many_arguments
)]
#[allow(clippy::too_many_lines)]
async fn dispatch_reverse_request<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    authority: &Authority,
    service: &HttpServicePlan,
    metrics: &ListenerMetrics,
    client_addr: Option<SocketAddr>,
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let method = request.method();
    let uri = request.uri();
    let Some(route) = service.select_route(Some(authority), uri, method) else {
        return send_h3_error(
            stream,
            StatusCode::NOT_FOUND,
            b"route not found\n",
            deadline,
        )
        .await
        .then_some(StatusCode::NOT_FOUND.as_u16());
    };
    if let Some(access) = &route.access
        && !access.authorizes(request.headers()).await
    {
        let headers = [(http::header::WWW_AUTHENTICATE, access.challenge().clone())];
        return send_h3_response(
            stream,
            StatusCode::UNAUTHORIZED,
            &headers,
            Bytes::new(),
            false,
            deadline,
        )
        .await
        .then_some(StatusCode::UNAUTHORIZED.as_u16());
    }

    match &route.action {
        HttpActionPlan::Fixed(response) => {
            let headers = response.headers.to_vec();
            return send_h3_response(
                stream,
                StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                &headers,
                response.body.clone(),
                *method == Method::HEAD,
                deadline,
            )
            .await
            .then_some(response.status);
        }
        HttpActionPlan::Redirect(redirect) => {
            let Some(location) = h3_redirect_location(&redirect.location, authority, uri) else {
                return send_h3_error(
                    stream,
                    StatusCode::BAD_REQUEST,
                    b"redirect location is invalid\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_REQUEST.as_u16());
            };
            let mut headers = redirect.headers.to_vec();
            headers.push((http::header::LOCATION, location));
            return send_h3_response(
                stream,
                StatusCode::from_u16(redirect.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                &headers,
                Bytes::new(),
                *method == Method::HEAD,
                deadline,
            )
            .await
            .then_some(redirect.status);
        }
        HttpActionPlan::Static(files) => {
            return send_h3_static_request(stream, files, request, deadline).await;
        }
        HttpActionPlan::Proxy(proxy) => {
            if method.as_str().eq_ignore_ascii_case("PURGE")
                && proxy
                    .policy
                    .cache
                    .as_ref()
                    .is_some_and(|cache| cache.purge_access.is_some())
            {
                return purge_h3_cache(
                    stream,
                    request,
                    authority,
                    Arc::clone(proxy.policy.cache.as_ref().expect("checked cache plan")),
                    deadline,
                )
                .await;
            }
            let request_header_decisions =
                match h3_request_header_decisions(request, authority, &proxy.policy, client_addr) {
                    Ok(decisions) => decisions,
                    Err(_) => {
                        return send_h3_error(
                            stream,
                            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                            b"request header policy expansion failed\n",
                            deadline,
                        )
                        .await
                        .then_some(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.as_u16());
                    }
                };
            if proxy.pool.h3().is_none() {
                return send_h3_error(
                    stream,
                    StatusCode::BAD_GATEWAY,
                    b"HTTP/3 reverse routes require an exact HTTP/3 upstream pool\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::BAD_GATEWAY.as_u16());
            }
            let body_limit = match (
                service.max_request_body_bytes(),
                route.policy.max_request_body_bytes,
            ) {
                (Some(service_limit), Some(route_limit)) => service_limit.min(route_limit),
                _ => {
                    return send_h3_error(
                        stream,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        b"HTTP/3 route has no bounded request body\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::INTERNAL_SERVER_ERROR.as_u16());
                }
            };
            let content_length = match request_content_length(request.headers()) {
                Ok(length) => length,
                Err(()) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_REQUEST,
                        b"request content-length is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_REQUEST.as_u16());
                }
            };
            if content_length.is_some_and(|length| length > body_limit) {
                stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
                return send_h3_error(
                    stream,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    b"request body exceeds the configured limit\n",
                    deadline,
                )
                .await
                .then_some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
            }
            let body = match recv_reverse_body(
                stream,
                body_limit,
                content_length,
                deadline,
                &mut shutdown,
            )
            .await
            {
                Ok(body) => body,
                Err(ReverseBodyError::TooLarge) => {
                    return send_h3_error(
                        stream,
                        StatusCode::PAYLOAD_TOO_LARGE,
                        b"request body exceeds the configured limit\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
                }
                Err(ReverseBodyError::Invalid) => {
                    return send_h3_error(
                        stream,
                        StatusCode::BAD_REQUEST,
                        b"request body is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::BAD_REQUEST.as_u16());
                }
                Err(ReverseBodyError::Timeout) => {
                    return send_h3_error(
                        stream,
                        StatusCode::GATEWAY_TIMEOUT,
                        b"request timed out\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::GATEWAY_TIMEOUT.as_u16());
                }
                Err(ReverseBodyError::Cancelled) => return None,
            };
            let _ = metrics.record_bytes_received(u64::try_from(body.len()).unwrap_or(u64::MAX));
            let mut cache_transaction = None;
            if body.is_empty()
                && let Some(cache) = proxy.policy.cache.as_ref()
                && cache.allows_method(method)
                && !cache_request_bypasses_cache(request.headers())
            {
                match start_h3_cache(cache, request, authority, metrics).await {
                    H3CacheDecision::Bypass => {}
                    H3CacheDecision::Respond(response) => {
                        let status = conditional_h3_cache_status(request, &response);
                        return send_h3_cached_response(stream, request, *response, deadline)
                            .await
                            .then_some(status.as_u16());
                    }
                    H3CacheDecision::OnlyIfCached => {
                        return send_h3_error(
                            stream,
                            StatusCode::GATEWAY_TIMEOUT,
                            b"cache entry is unavailable\n",
                            deadline,
                        )
                        .await
                        .then_some(StatusCode::GATEWAY_TIMEOUT.as_u16());
                    }
                    H3CacheDecision::Continue(transaction) => {
                        cache_transaction = Some(*transaction);
                    }
                }
            }
            dispatch_h3_upstream_request(
                stream,
                request,
                authority,
                proxy,
                &request_header_decisions,
                body,
                metrics,
                deadline,
                shutdown,
                cache_transaction,
            )
            .await
        }
    }
}

#[derive(Clone, Copy)]
enum ReverseBodyError {
    TooLarge,
    Invalid,
    Timeout,
    Cancelled,
}

async fn recv_reverse_body<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    limit: u64,
    expected_length: Option<u64>,
    deadline: Instant,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Bytes, ReverseBodyError>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                return Err(ReverseBodyError::Cancelled);
            }
            result = timeout_at(deadline, stream.recv_data()) => match result {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(_)) => return Err(ReverseBodyError::Invalid),
                Err(_) => return Err(ReverseBodyError::Timeout),
            },
        };
        let Some(chunk) = next else {
            break;
        };
        let mut chunk = chunk;
        let chunk_length = chunk.remaining();
        let chunk = chunk.copy_to_bytes(chunk_length);
        let next_length = body
            .len()
            .checked_add(chunk.len())
            .and_then(|length| u64::try_from(length).ok())
            .ok_or(ReverseBodyError::TooLarge)?;
        if next_length > limit {
            stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
            return Err(ReverseBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    if expected_length.is_some_and(|length| length != u64::try_from(body.len()).unwrap_or(u64::MAX))
    {
        return Err(ReverseBodyError::Invalid);
    }
    Ok(Bytes::from(body))
}

pub(super) async fn send_h3_error<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    status: StatusCode,
    body: &[u8],
    deadline: Instant,
) -> bool
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    send_h3_response(
        stream,
        status,
        &[],
        Bytes::copy_from_slice(body),
        false,
        deadline,
    )
    .await
}
