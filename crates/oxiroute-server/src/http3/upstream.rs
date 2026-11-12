#[allow(clippy::wildcard_imports)]
use super::*;
use crate::routing::QueueTag;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn dispatch_h3_upstream_request(
    stream: &mut H3Stream,
    request: &Request<()>,
    authority: &Authority,
    proxy: &ProxyActionPlan,
    upstream: &Upstream<'_>,
    request_header_decisions: &[RequestHeaderDecision],
    body: Bytes,
    metrics: &ListenerMetrics,
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
    mut cache_transaction: Option<CacheTransaction>,
) -> Option<u16> {
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
    // An answer is sent as it arrives unless the cache needs all of it or the client wants no body.
    let may_stream = cache_transaction.is_none() && request.method() != Method::HEAD;

    for attempt in 0..max_attempts {
        let mut selected = if let Some(server) = retry_server.take() {
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    return None;
                }
                result = timeout_at(deadline, proxy.pool.select_server_endpoint(&server, QueueTag::default())) => {
                    match result {
                        Ok(Ok(selected)) => selected,
                        Ok(Err(_)) => {
                            last_error = H3UpstreamError::PoolUnavailable;
                            break;
                        }
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
                result = timeout_at(deadline, proxy.pool.select_endpoint(&attempted, QueueTag::default())) => {
                    match result {
                        Ok(Ok(selected)) => selected,
                        Ok(Err(_)) => {
                            last_error = H3UpstreamError::PoolUnavailable;
                            break;
                        }
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
            &proxy.policy.upstream_host,
            Some(authority),
        ) else {
            last_error = H3UpstreamError::Protocol;
            break;
        };
        let Ok(mut upstream_request) = upstream.build_request(
            &proxy.policy,
            request,
            body.len(),
            &selected_host,
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
            match upstream
                .exchange(
                    &mut selected,
                    &upstream_request,
                    &body,
                    (remaining, deadline),
                    &shutdown,
                    may_stream,
                )
                .await
            {
                Ok(value) => {
                    response = Some(value);
                    break;
                }
                Err(error) if error.reconnectable() && selected.has_address_fallback() => {
                    last_error = error;
                }
                Err(error) => {
                    last_error = error;
                    break;
                }
            }
        }
        if let Some(response) = response {
            if proxy.policy.retries_on_status(response.status().as_u16()) {
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
        let retry = (retry_safe || last_error.request_not_sent())
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

    if let Some(response) = stale_h3_response(&mut cache_transaction).await {
        let status = conditional_h3_cache_status(request, &response);
        return send_h3_cached_response(stream, request, response, deadline)
            .await
            .then_some(status.as_u16());
    }
    if let Some(transaction) = cache_transaction.as_mut() {
        transaction.complete_without_store();
    }
    let head = request.method() == Method::HEAD;
    send_h3_upstream_failure(stream, &last_error, &proxy.pool, head, deadline).await
}

async fn send_h3_upstream_response(
    stream: &mut H3Stream,
    request: &Request<()>,
    answer: Answer<'_>,
    proxy: &ProxyActionPlan,
    metrics: &ListenerMetrics,
    deadline: Instant,
    cache_transaction: &mut Option<CacheTransaction>,
) -> Option<u16> {
    match answer {
        Answer::Whole(response) => {
            send_h3_whole_response(
                stream,
                request,
                *response,
                proxy,
                metrics,
                deadline,
                cache_transaction,
            )
            .await
        }
        Answer::Open(open) => {
            send_h3_streamed_response(stream, *open, proxy, metrics, deadline).await
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn send_h3_whole_response(
    stream: &mut H3Stream,
    request: &Request<()>,
    response: crate::http3::upstream_plan::H3UpstreamResponse,
    proxy: &ProxyActionPlan,
    metrics: &ListenerMetrics,
    deadline: Instant,
    cache_transaction: &mut Option<CacheTransaction>,
) -> Option<u16> {
    let status = response.status;
    let mut headers = response.headers;
    let body = response.body;
    let trailers = response.trailers;
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
    let head = request.method() == Method::HEAD;
    let send_body = !head && !body_forbidden(status) && !body.is_empty();
    if send_body {
        let _ = metrics.record_bytes_sent(body_length);
    }
    let trailers = trailers.filter(|_| !head);
    if !send_h3_upstream_parts(stream, response_head, body, !send_body, trailers, deadline).await {
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

/// Sends an answer whose body is still with the origin: the head first, then each piece as it arrives, then the
/// trailers. The stream ends when the body does; an origin that fails after the head leaves the response unfinished,
/// which resets the stream.
async fn send_h3_streamed_response(
    stream: &mut H3Stream,
    mut open: OpenAnswer<'_>,
    proxy: &ProxyActionPlan,
    metrics: &ListenerMetrics,
    deadline: Instant,
) -> Option<u16> {
    let status = open.status;
    let mut headers = std::mem::take(&mut open.headers);
    if apply_response_policy_map(status, &mut headers, &proxy.policy).is_err()
        || sanitize_h3_streamed_headers(&mut headers, status).is_err()
    {
        let _ = send_h3_error(
            stream,
            StatusCode::BAD_GATEWAY,
            b"HTTP/3 upstream response headers are invalid\n",
            deadline,
        )
        .await;
        return Some(StatusCode::BAD_GATEWAY.as_u16());
    }
    let declared = request_content_length(&headers).ok().flatten();
    let mut head = Response::new(());
    *head.status_mut() = status;
    *head.headers_mut() = headers;
    if !within(deadline, stream.send_response(head)).await {
        return None;
    }
    let mut sent = 0_u64;
    loop {
        let chunk = match timeout_at(deadline, open.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(_)) | Err(_) => return Some(status.as_u16()),
        };
        sent += chunk.len() as u64;
        if declared.is_some_and(|declared| sent > declared)
            || !within(deadline, stream.send_data(chunk)).await
        {
            return Some(status.as_u16());
        }
    }
    let _ = metrics.record_bytes_sent(sent);
    if declared.is_some_and(|declared| sent != declared) {
        return Some(status.as_u16());
    }
    let trailers = match timeout_at(deadline, open.trailers()).await {
        Ok(Ok(trailers)) => trailers.filter(|trailers| !trailers.is_empty()),
        Ok(Err(_)) | Err(_) => return Some(status.as_u16()),
    };
    open.release().await;
    if let Some(mut trailers) = trailers
        && (sanitize_h3_trailers(&mut trailers).is_err()
            || !within(deadline, stream.send_trailers(trailers)).await)
    {
        return Some(status.as_u16());
    }
    stream.finish().ok()?;
    Some(status.as_u16())
}

/// Sends head, body and trailers and finishes the stream; a response without trailers goes out in one write.
async fn send_h3_upstream_parts(
    stream: &mut H3Stream,
    head: Response<()>,
    body: Bytes,
    head_only: bool,
    trailers: Option<HeaderMap>,
    deadline: Instant,
) -> bool {
    let Some(mut trailers) = trailers else {
        return within(deadline, stream.send_full_response(head, body, head_only)).await;
    };
    if !within(deadline, stream.send_response(head)).await {
        return false;
    }
    if !head_only && !within(deadline, stream.send_data(body)).await {
        return false;
    }
    sanitize_h3_trailers(&mut trailers).is_ok()
        && within(deadline, stream.send_trailers(trailers)).await
        && stream.finish().is_ok()
}

pub(super) fn build_h3_upstream_request(
    request: &Request<()>,
    body_length: usize,
    selected_host: &HeaderValue,
    policy: &ProxyPolicyPlan,
    request_header_decisions: &[RequestHeaderDecision],
) -> Result<Request<()>, ()> {
    let selected_authority = selected_host
        .to_str()
        .map_err(|_| ())?
        .parse::<Authority>()
        .map_err(|_| ())?;
    let uri = Uri::builder()
        .scheme("https")
        .authority(selected_authority.as_str())
        .path_and_query(upstream_path(request, policy)?)
        .build()
        .map_err(|_| ())?;
    let content_length = HeaderValue::from_str(&body_length.to_string()).map_err(|_| ())?;
    let headers = upstream_headers(
        request,
        selected_host,
        request_header_decisions,
        |headers| {
            headers.insert(CONTENT_LENGTH, content_length);
        },
    )?;
    let mut output = Request::builder()
        .method(request.method().clone())
        .uri(uri)
        .body(())
        .map_err(|_| ())?;
    *output.version_mut() = http::Version::HTTP_3;
    *output.headers_mut() = headers;
    Ok(output)
}

/// The request's path and query as the upstream gets them, after the route's rewrite.
pub(super) fn upstream_path(request: &Request<()>, policy: &ProxyPolicyPlan) -> Result<String, ()> {
    let mut uri = request.uri().clone();
    if let Some(rewrite) = &policy.upstream_path_rewrite {
        rewrite_upstream_path(&mut uri, rewrite).map_err(|_| ())?;
    }
    Ok(uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str())
        .to_owned())
}

/// The request's fields as an upstream gets them: hop-by-hop fields and `Host` removed, the fields the transport
/// owns set by `transport`, then the route's header policy applied.
pub(super) fn upstream_headers(
    request: &Request<()>,
    selected_host: &HeaderValue,
    request_header_decisions: &[RequestHeaderDecision],
    transport: impl FnOnce(&mut HeaderMap),
) -> Result<HeaderMap, ()> {
    let mut headers = request.headers().clone();
    strip_h3_hop_by_hop_headers(&mut headers)?;
    headers.remove(HOST);
    transport(&mut headers);
    apply_h3_header_mutations(&mut headers, request_header_decisions, selected_host)?;
    Ok(headers)
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
