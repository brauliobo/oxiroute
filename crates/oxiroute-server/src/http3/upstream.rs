#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn dispatch_h3_upstream_request<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    authority: &Authority,
    proxy: &ProxyActionPlan,
    request_header_decisions: &[RequestHeaderDecision],
    body: Bytes,
    metrics: &ListenerMetrics,
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
    mut cache_transaction: Option<CacheTransaction>,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let Some(h3) = proxy.pool.h3() else {
        return send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream policy is unavailable\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_GATEWAY.as_u16());
    };
    let Some(server_name) = h3.server_name() else {
        return send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream SNI is unavailable\n",
            deadline,
        )
        .await
        .then_some(StatusCode::BAD_GATEWAY.as_u16());
    };
    let mut attempted = Vec::new();
    let mut retry_server: Option<String> = None;
    let max_attempts = usize::from(proxy.policy.max_retries).saturating_add(1);
    let retry_safe = proxy.policy.request_is_replay_safe(
        request.method(),
        request.headers(),
        body.is_empty(),
        true,
    );
    let mut last_error = H3UpstreamError::Connect;

    for attempt in 0..max_attempts {
        let mut selected = if let Some(server) = retry_server.take() {
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                result = timeout_at(deadline, proxy.pool.select_server_endpoint(&server)) => {
                    match result {
                        Ok(Ok(selected)) => selected,
                        Ok(Err(_)) => break,
                        Err(_) => {
                            last_error = H3UpstreamError::Timeout;
                            break;
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                result = timeout_at(deadline, proxy.pool.select_endpoint(&attempted)) => {
                    match result {
                        Ok(Ok(selected)) => selected,
                        Ok(Err(_)) => break,
                        Err(_) => {
                            last_error = H3UpstreamError::Timeout;
                            break;
                        }
                    }
                }
            }
        };
        let Ok(Some(selected_host)) = selected_upstream_host(
            selected.endpoint(),
            proxy.policy.upstream_host.clone(),
            Some(authority),
        ) else {
            last_error = H3UpstreamError::Protocol;
            break;
        };
        let Ok(mut upstream_request) = build_h3_upstream_request(
            request,
            body.len(),
            &selected_host,
            &proxy.policy,
            request_header_decisions,
        ) else {
            last_error = H3UpstreamError::Protocol;
            break;
        };
        if let Some(transaction) = cache_transaction.as_ref() {
            if transaction.request().method == Method::HEAD {
                *upstream_request.method_mut() = Method::GET;
            }
            transaction.apply_validators(upstream_request.headers_mut());
        }
        attempted.push(selected.server_name().to_owned());
        let mut response = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                last_error = H3UpstreamError::Timeout;
                break;
            }
            let Ok(address) = selected.prepare_h3_address(remaining).await else {
                last_error = H3UpstreamError::Connect;
                break;
            };
            match h3
                .request(
                    address,
                    server_name,
                    upstream_request.clone(),
                    body.clone(),
                    deadline,
                    shutdown.clone(),
                )
                .await
            {
                Ok(value) => {
                    response = Some(value);
                    break;
                }
                Err(error) if error.retryable() && selected.has_address_fallback() => {
                    last_error = error;
                }
                Err(error) => {
                    last_error = error;
                    break;
                }
            }
        }
        if let Some(response) = response {
            if proxy.policy.retries_on_status(response.status.as_u16()) {
                selected
                    .observation()
                    .record_passive_failure(HealthFailure::UnexpectedStatus);
                let retry_target = proxy.policy.target_for_retry(attempted.len());
                let target_available = match retry_target {
                    HttpRetryTarget::SameServer => attempted.last().is_some(),
                    HttpRetryTarget::NextServer => proxy.pool.has_unattempted(&attempted),
                };
                let retry = retry_safe
                    && attempt.saturating_add(1) < max_attempts
                    && target_available
                    && Instant::now() < deadline;
                if retry {
                    let delay = proxy.policy.next_retry_delay();
                    if !delay.is_zero() {
                        if delay > deadline.saturating_duration_since(Instant::now()) {
                            return send_h3_upstream_response(
                                stream,
                                request,
                                response,
                                proxy,
                                metrics,
                                deadline,
                                &mut cache_transaction,
                            )
                            .await;
                        }
                        tokio::select! {
                            changed = shutdown.changed() => {
                                let _ = changed;
                                return None;
                            }
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                    if retry_target == HttpRetryTarget::SameServer {
                        retry_server = attempted.last().cloned();
                    }
                    continue;
                }
            } else {
                selected.observation().record_passive_success();
            }
            return send_h3_upstream_response(
                stream,
                request,
                response,
                proxy,
                metrics,
                deadline,
                &mut cache_transaction,
            )
            .await;
        }

        if let Some(failure) = h3_passive_failure(&last_error) {
            selected.observation().record_passive_failure(failure);
        }

        let trigger = h3_retry_trigger(&last_error);
        let retry_target = proxy.policy.target_for_retry(attempted.len());
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => attempted.last().is_some(),
            HttpRetryTarget::NextServer => proxy.pool.has_unattempted(&attempted),
        };
        let retry = retry_safe
            && attempt.saturating_add(1) < max_attempts
            && target_available
            && proxy.policy.retries_on(trigger)
            && last_error.retryable()
            && Instant::now() < deadline;
        if !retry {
            break;
        }
        let delay = proxy.policy.next_retry_delay();
        if !delay.is_zero() {
            if delay > deadline.saturating_duration_since(Instant::now()) {
                break;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                () = tokio::time::sleep(delay) => {}
            }
        }
        if retry_target == HttpRetryTarget::SameServer {
            retry_server = attempted.last().cloned();
        }
    }

    let status = if matches!(last_error, H3UpstreamError::Timeout) {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    };
    if let Some(response) = stale_h3_response(&mut cache_transaction).await {
        let status = conditional_h3_cache_status(request, &response);
        return send_h3_cached_response(stream, request, response, deadline)
            .await
            .then_some(status.as_u16());
    }
    if let Some(transaction) = cache_transaction.as_mut() {
        transaction.complete_without_store();
    }
    send_h3_error(
        stream,
        status,
        b"HTTP/3 upstream request failed\n",
        deadline,
    )
    .await
    .then_some(status.as_u16())
}

fn h3_retry_trigger(error: &H3UpstreamError) -> HttpRetryTrigger {
    match error {
        H3UpstreamError::Timeout => HttpRetryTrigger::ConnectTimeout,
        H3UpstreamError::RefusedStream => HttpRetryTrigger::RefusedStream,
        H3UpstreamError::Connect
        | H3UpstreamError::Protocol
        | H3UpstreamError::Cancelled
        | H3UpstreamError::ResponseBodyTooLarge
        | H3UpstreamError::RequestBodyTooLarge
        | H3UpstreamError::ResourceExhausted
        | H3UpstreamError::MissingServerName => HttpRetryTrigger::ConnectFailure,
    }
}

fn h3_passive_failure(error: &H3UpstreamError) -> Option<HealthFailure> {
    match error {
        H3UpstreamError::Connect => Some(HealthFailure::ConnectFailed),
        H3UpstreamError::Timeout => Some(HealthFailure::Timeout),
        H3UpstreamError::RefusedStream | H3UpstreamError::Protocol => {
            Some(HealthFailure::ProtocolError)
        }
        H3UpstreamError::Cancelled
        | H3UpstreamError::ResponseBodyTooLarge
        | H3UpstreamError::RequestBodyTooLarge
        | H3UpstreamError::ResourceExhausted
        | H3UpstreamError::MissingServerName => None,
    }
}

#[allow(clippy::too_many_lines)]
async fn send_h3_upstream_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    request: &Request<()>,
    response: crate::http3_upstream::H3UpstreamResponse,
    proxy: &ProxyActionPlan,
    metrics: &ListenerMetrics,
    deadline: Instant,
    cache_transaction: &mut Option<CacheTransaction>,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let status = response.status;
    let mut headers = response.headers;
    let body = response.body;
    let mut trailers = response.trailers;
    if apply_response_policy_map(status, &mut headers, &proxy.policy).is_err() {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        let _ = send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream response headers are invalid\n",
            deadline,
        )
        .await;
        return Some(StatusCode::BAD_GATEWAY.as_u16());
    }
    let body_length = u64::try_from(body.len()).unwrap_or(u64::MAX);
    if sanitize_h3_response_headers(
        &mut headers,
        status,
        body_length,
        request.method() == Method::HEAD,
    )
    .is_err()
    {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        let _ = send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream response cannot be represented safely\n",
            deadline,
        )
        .await;
        return Some(StatusCode::BAD_GATEWAY.as_u16());
    }

    if cache_transaction
        .as_ref()
        .is_some_and(CacheTransaction::is_revalidation)
    {
        if status == StatusCode::NOT_MODIFIED {
            let transaction = cache_transaction
                .as_mut()
                .expect("revalidation has a cache transaction");
            let timing = ResponseTiming {
                request_started: transaction.request().request_started,
                response_received: transaction.now(),
                response_received_wall: std::time::SystemTime::now(),
            };
            let cached = transaction.finish_revalidation(&headers, timing).await;
            let cached_status = conditional_h3_cache_status(request, &cached);
            return send_h3_cached_response(stream, request, cached, deadline)
                .await
                .then_some(cached_status.as_u16());
        }
        if status.is_server_error()
            && let Some(cached) = stale_h3_response(cache_transaction).await
        {
            let cached_status = conditional_h3_cache_status(request, &cached);
            return send_h3_cached_response(stream, request, cached, deadline)
                .await
                .then_some(cached_status.as_u16());
        }
    }

    let mut prepared = None;
    if let Some(transaction) = cache_transaction.as_mut() {
        let tags = transaction
            .surrogate_header()
            .map_or_else(Vec::new, |header| h3_surrogate_tags(&headers, header));
        let tags_valid = transaction.cache_tags_within_limits(&tags);
        if trailers.as_ref().is_none_or(HeaderMap::is_empty)
            && tags_valid
            && h3_cache_representation_valid(status, &headers, body.len())
        {
            let tag_refs = tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
            let timing = ResponseTiming {
                request_started: transaction.request().request_started,
                response_received: transaction.now(),
                response_received_wall: std::time::SystemTime::now(),
            };
            prepared = transaction
                .prepare_response(status, &headers, body.clone(), timing, &tag_refs)
                .ok();
        }
        if prepared.is_none() {
            transaction.complete_without_store();
        }
    }
    let mut response_head = Response::new(());
    *response_head.status_mut() = status;
    *response_head.headers_mut() = headers;
    if !matches!(
        timeout_at(deadline, stream.send_response(response_head)).await,
        Ok(Ok(()))
    ) {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return None;
    }
    let body_forbidden = matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    if request.method() != Method::HEAD && !body_forbidden && !body.is_empty() {
        let _ = metrics.record_bytes_sent(body_length);
        if !matches!(
            timeout_at(deadline, stream.send_data(body)).await,
            Ok(Ok(()))
        ) {
            if let Some(transaction) = cache_transaction.as_mut() {
                transaction.cancel();
            }
            return None;
        }
    }
    if request.method() != Method::HEAD
        && let Some(mut trailers) = trailers.take()
        && (sanitize_h3_trailers(&mut trailers).is_err()
            || !matches!(
                timeout_at(deadline, stream.send_trailers(trailers)).await,
                Ok(Ok(()))
            ))
    {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return None;
    }
    if !matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(()))) {
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.cancel();
        }
        return None;
    }
    if let Some(entry) = prepared
        && let Some(transaction) = cache_transaction.as_mut()
    {
        let _ = transaction.admit(entry).await;
    }
    Some(status.as_u16())
}

fn build_h3_upstream_request(
    request: &Request<()>,
    body_length: usize,
    selected_host: &HeaderValue,
    policy: &ProxyPolicyPlan,
    request_header_decisions: &[RequestHeaderDecision],
) -> Result<Request<()>, ()> {
    let mut uri = request.uri().clone();
    if let Some(rewrite) = &policy.upstream_path_rewrite {
        rewrite_upstream_path(&mut uri, rewrite).map_err(|_| ())?;
    }
    let selected_authority = selected_host
        .to_str()
        .map_err(|_| ())?
        .parse::<Authority>()
        .map_err(|_| ())?;
    let path = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    let uri = Uri::builder()
        .scheme("https")
        .authority(selected_authority.as_str())
        .path_and_query(path)
        .build()
        .map_err(|_| ())?;
    let mut headers = request.headers().clone();
    for name in [
        HOST,
        CONNECTION,
        KEEP_ALIVE,
        PROXY_CONNECTION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&body_length.to_string()).map_err(|_| ())?,
    );
    apply_h3_header_mutations(&mut headers, request_header_decisions, selected_host)?;
    let mut output = Request::builder()
        .method(request.method().clone())
        .uri(uri)
        .body(())
        .map_err(|_| ())?;
    *output.version_mut() = http::Version::HTTP_3;
    *output.headers_mut() = headers;
    Ok(output)
}

pub(super) fn apply_h3_header_mutations(
    headers: &mut HeaderMap,
    decisions: &[RequestHeaderDecision],
    selected_upstream_host: &HeaderValue,
) -> Result<(), ()> {
    for decision in decisions {
        match decision {
            RequestHeaderDecision::Remove(name) => {
                headers.remove(name);
            }
            RequestHeaderDecision::Set { name, value } => {
                let Some(value) = value
                    .complete(Some(selected_upstream_host))
                    .map_err(|_| ())?
                else {
                    continue;
                };
                headers.insert(name.clone(), value);
            }
        }
    }
    Ok(())
}

pub(super) fn h3_request_header_decisions(
    request: &Request<()>,
    authority: &Authority,
    policy: &ProxyPolicyPlan,
    client_addr: Option<SocketAddr>,
) -> Result<Box<[RequestHeaderDecision]>, RequestPolicyError> {
    let context = RequestPolicyContext {
        authority: Some(authority),
        downstream_scheme: "https",
        client_ip: client_addr.map(|address| address.ip()),
        incoming_headers: request.headers(),
    };
    let mut decisions = Vec::with_capacity(policy.request_headers.len());
    for mutation in &policy.request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        decisions.push(decide_request_header(mutation, context)?);
    }
    Ok(decisions.into_boxed_slice())
}
