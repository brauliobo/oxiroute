#[allow(clippy::wildcard_imports)]
use super::*;
use crate::observability::access_record::{Correlation, Count, Http3Access, SafeId};

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_reverse_request(
    mut stream: H3Stream,
    service: Arc<HttpServicePlan>,
    metrics: ListenerMetrics,
    client_addr: Option<SocketAddr>,
    downstream_timeouts: DownstreamTimeoutPolicy,
    origin: Arc<OriginClient>,
    mut shutdown: watch::Receiver<bool>,
    request_slots: Arc<Semaphore>,
) {
    let request = match stream.read_head().await {
        Ok(request) => request,
        Err(HeadError::FieldSectionTooLarge { .. }) => {
            let deadline = Instant::now() + Duration::from_secs(1);
            let _ = send_h3_error(
                &mut stream,
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                b"request headers exceed the HTTP/3 limit\n",
                deadline,
            )
            .await;
            return;
        }
        Err(_) => return,
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
    let authority = request
        .uri()
        .authority()
        .cloned()
        .expect("the HTTP/3 layer delivers requests with an authority");
    let response_status = if is_unambiguous_http_path(uri.path()) {
        let dispatch_shutdown = shutdown.clone();
        tokio::select! {
            _ = shutdown.changed() => None,
            response = dispatch_reverse_request(
                &mut stream,
                &request,
                &authority,
                &service,
                &metrics,
                client_addr,
                &origin,
                deadline,
                dispatch_shutdown,
            ) => response,
        }
    } else {
        send_h3_error(
            &mut stream,
            StatusCode::BAD_REQUEST,
            b"request path is invalid\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_REQUEST.as_u16())
    };

    let result = response_status.map_or(HttpOperationResult::Cancelled, |status| {
        HttpOperationResult::from_status(Some(status))
    });
    if let Some(access_log) = service.access_log()
        && let Err(error) = access_log.write_record(&Http3Access {
            correlation_id: Correlation(None),
            duration_ms: Count(started_at.elapsed().as_millis()),
            host: Some(authority.host().into()),
            method: method.as_str().into(),
            protocol: SafeId("h3"),
            service: access_log.service().into(),
            status: response_status,
            timestamp_unix_ms: unix_time_ms(),
        })
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
async fn dispatch_reverse_request(
    stream: &mut H3Stream,
    request: &Request<()>,
    authority: &Authority,
    service: &HttpServicePlan,
    metrics: &ListenerMetrics,
    client_addr: Option<SocketAddr>,
    origin: &OriginClient,
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
) -> Option<u16> {
    let method = request.method();
    let uri = request.uri();
    let Some(route) = service.select_route(Some(authority), uri, method).cloned() else {
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
            let head = response.h3_head.as_ref()?;
            let head_only = *method == Method::HEAD;
            return within(
                deadline,
                stream.send_prepared(head, response.body.clone(), head_only),
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
            let upstream = match Upstream::select(proxy, route.policy, origin) {
                Ok(upstream) => upstream,
                Err(message) => {
                    return send_h3_error(stream, StatusCode::BAD_GATEWAY, message, deadline)
                        .await
                        .then_some(StatusCode::BAD_GATEWAY.as_u16());
                }
            };
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
                stream.stop_sending(H3Code::H3_REQUEST_CANCELLED);
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
                &upstream,
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

async fn recv_reverse_body(
    stream: &mut H3Stream,
    limit: u64,
    expected_length: Option<u64>,
    deadline: Instant,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Bytes, ReverseBodyError> {
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
        let next_length = body
            .len()
            .checked_add(chunk.len())
            .and_then(|length| u64::try_from(length).ok())
            .ok_or(ReverseBodyError::TooLarge)?;
        if next_length > limit {
            stream.stop_sending(H3Code::H3_REQUEST_CANCELLED);
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

pub(super) async fn send_h3_error(
    stream: &mut H3Stream,
    status: StatusCode,
    body: &'static [u8],
    deadline: Instant,
) -> bool {
    send_h3_response(
        stream,
        status,
        &[],
        Bytes::from_static(body),
        false,
        deadline,
    )
    .await
}
