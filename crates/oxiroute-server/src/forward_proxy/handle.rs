#[allow(clippy::wildcard_imports)]
use super::*;

impl ForwardHttp1ServicePlan {
    // Keep the UDP relay future inline so cancellation remains owned by this tunnel task.
    #[allow(clippy::large_futures, clippy::too_many_lines)]
    pub async fn handle(
        self: &Arc<Self>,
        mut request: Request<Incoming>,
        client_addr: Option<SocketAddr>,
        mut shutdown: ShutdownWatch,
        lifecycle: Arc<ForwardConnectionLifecycle>,
        listener: crate::ListenerMetrics,
    ) -> Response<ForwardProxyBody> {
        let AuthorizedRequest {
            approved,
            authenticated,
            parsed,
            lifetime_deadline,
            ..
        } = match self
            .authorize_request(&request, Protocol::Http1, client_addr)
            .await
        {
            Ok(request) => request,
            Err(error) => return self.rejection(error),
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);

        match parsed {
            ParsedTarget::Tunnel(destination) => {
                let upstream = match timeout_at(
                    lifetime_deadline,
                    self.connect_tunnel_until(
                        &destination,
                        approved.socket_addresses(),
                        connect_deadline,
                    ),
                )
                .await
                {
                    Ok(Ok(upstream)) => upstream,
                    Ok(Err(error)) => return self.rejection(error),
                    Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
                };
                let upgrade = hyper::upgrade::on(&mut request);
                let idle_timeout = self.idle_timeout;
                let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
                if lifetime_timeout.is_zero() {
                    return self.rejection(RequestFailure::GatewayTimeout);
                }
                let tunnel_destination = approved.destination().authority();
                let completion = lifecycle.start();
                tokio::spawn(async move {
                    let _completion = completion;
                    let upgraded = tokio::select! {
                        result = upgrade => result.ok(),
                        _ = shutdown.changed() => None,
                    };
                    if let Some(upgraded) = upgraded {
                        let Ok(tunnel) = BoundedTunnel::new(TunnelLimits {
                            idle_timeout,
                            lifetime_timeout,
                            ..TunnelLimits::default()
                        }) else {
                            return;
                        };
                        let outcome = tokio::select! {
                            outcome = tunnel.relay(TokioIo::new(upgraded), upstream) => Some(outcome),
                            _ = shutdown.changed() => None,
                        };
                        match outcome {
                            Some(outcome) => log::info!(
                                target: "oxiroute::forward_proxy",
                                "event=forward_tunnel protocol=h1 destination={} outcome={} bytes_left_to_right={} bytes_right_to_left={}",
                                tunnel_destination,
                                outcome.kind().as_str(),
                                outcome.stats().left_to_right,
                                outcome.stats().right_to_left,
                            ),
                            None => log::info!(
                                target: "oxiroute::forward_proxy",
                                "event=forward_tunnel protocol=h1 destination={tunnel_destination} outcome=cancelled bytes_left_to_right=0 bytes_right_to_left=0",
                            ),
                        }
                    }
                });
                response(StatusCode::OK, Bytes::new())
            }
            ParsedTarget::UdpTunnel(_) => {
                let socket = match timeout_at(
                    lifetime_deadline,
                    self.connect_udp_until(approved.socket_addresses(), connect_deadline),
                )
                .await
                {
                    Ok(Ok(socket)) => socket,
                    Ok(Err(error)) => return self.rejection(error),
                    Err(_) => return self.rejection(RequestFailure::GatewayTimeout),
                };
                let upgrade = hyper::upgrade::on(&mut request);
                let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
                if lifetime_timeout.is_zero() {
                    return self.rejection(RequestFailure::GatewayTimeout);
                }
                let tunnel_destination = approved.destination().authority();
                let completion = lifecycle.start();
                let limits = TunnelLimits {
                    idle_timeout: self.idle_timeout,
                    lifetime_timeout,
                    ..TunnelLimits::default()
                };
                tokio::spawn(async move {
                    let _completion = completion;
                    let upgraded = tokio::select! {
                        result = upgrade => result.ok(),
                        _ = shutdown.changed() => None,
                    };
                    let Some(upgraded) = upgraded else {
                        return;
                    };
                    let outcome = relay_udp(TokioIo::new(upgraded), socket, limits, shutdown).await;
                    log::info!(
                        target: "oxiroute::forward_proxy",
                        "event=forward_udp_tunnel protocol=h1 destination={} outcome={} bytes_client_to_udp={} bytes_udp_to_client={}",
                        tunnel_destination,
                        outcome.end.as_str(),
                        outcome.stats.client_to_udp,
                        outcome.stats.udp_to_client,
                    );
                });
                switching_protocols_response()
            }
            ParsedTarget::Forward(target) => {
                self.handle_forward_request(
                    request,
                    target,
                    approved,
                    authenticated,
                    lifetime_deadline,
                    connect_deadline,
                    shutdown,
                    listener,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn handle_forward_request(
        &self,
        mut request: Request<Incoming>,
        target: oxiroute_forward_proxy::ForwardTarget,
        approved: ApprovedDestination,
        authenticated: bool,
        lifetime_deadline: Instant,
        connect_deadline: Instant,
        mut shutdown: ShutdownWatch,
        listener: crate::ListenerMetrics,
    ) -> Response<ForwardProxyBody> {
        if request.body().size_hint().upper().is_some_and(|length| {
            length > u64::try_from(self.max_request_body_bytes).unwrap_or(u64::MAX)
        }) {
            return self.rejection(RequestFailure::PayloadTooLarge);
        }
        if request.method().as_str().eq_ignore_ascii_case("PURGE")
            && self
                .cache
                .as_ref()
                .is_some_and(|plan| plan.purge_access.is_some())
        {
            return self.handle_forward_cache_purge(request, &target).await;
        }

        let mut cache_transaction = match self
            .prepare_forward_cache(&request, &target, authenticated, &listener)
            .await
        {
            ForwardCacheDecision::Bypass => None,
            ForwardCacheDecision::Respond(response) => return response,
            ForwardCacheDecision::Continue(transaction) => Some(transaction),
        };
        let ConnectedHttp {
            stream: upstream,
            via_peer,
        } = match timeout_at(
            lifetime_deadline,
            self.connect_http_with_peers(
                &target.destination,
                target.scheme,
                approved.socket_addresses(),
                connect_deadline,
            ),
        )
        .await
        {
            Ok(Ok(upstream)) => upstream,
            Ok(Err(error)) => {
                let class = if error == RequestFailure::GatewayTimeout {
                    ForwardCacheFailure::UpstreamTimeout
                } else {
                    ForwardCacheFailure::UpstreamConnect
                };
                return self
                    .forward_failure(&mut cache_transaction, error, class)
                    .await;
            }
            Err(_) => {
                return self
                    .forward_failure(
                        &mut cache_transaction,
                        RequestFailure::GatewayTimeout,
                        ForwardCacheFailure::UpstreamTimeout,
                    )
                    .await;
            }
        };
        let request_uri = if via_peer && target.scheme == ForwardScheme::Http {
            let Some(uri) = absolute_form(&target) else {
                return self
                    .forward_failure(
                        &mut cache_transaction,
                        RequestFailure::BadRequest,
                        ForwardCacheFailure::Local,
                    )
                    .await;
            };
            uri
        } else {
            target.origin_form.clone()
        };
        *request.uri_mut() = request_uri;
        let Ok(mut headers) = sanitize_request_headers(request.headers(), &target.destination)
        else {
            return self
                .forward_failure(
                    &mut cache_transaction,
                    RequestFailure::BadRequest,
                    ForwardCacheFailure::Local,
                )
                .await;
        };
        apply_header_policy(&mut headers, &self.header_policy);
        if let Some(transaction) = cache_transaction.as_ref() {
            transaction.apply_validators(&mut headers);
        }
        *request.headers_mut() = headers;
        let request_body_empty = request.body().size_hint().upper() == Some(0);
        let (parts, body) = request.into_parts();
        let (body, mut body_completion) = relay_request_body(
            body,
            self.max_request_body_bytes,
            self.idle_timeout,
            lifetime_deadline,
        );
        let request = Request::from_parts(parts, body);
        let handshake_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let (mut sender, connection) = match timeout_at(
            handshake_deadline,
            hyper::client::conn::http1::handshake(TokioIo::new(upstream)),
        )
        .await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(_)) => {
                return self
                    .forward_failure(
                        &mut cache_transaction,
                        RequestFailure::BadGateway,
                        ForwardCacheFailure::UpstreamProtocol,
                    )
                    .await;
            }
            Err(_) => {
                return self
                    .forward_failure(
                        &mut cache_transaction,
                        RequestFailure::GatewayTimeout,
                        ForwardCacheFailure::UpstreamTimeout,
                    )
                    .await;
            }
        };
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender.send_request(request);
        tokio::pin!(response);
        let mut body_complete = request_body_empty;
        let response_idle = tokio::time::sleep(self.idle_timeout);
        tokio::pin!(response_idle);
        let mut upstream_response = loop {
            tokio::select! {
                biased;
                result = &mut body_completion, if !body_complete => {
                    body_complete = true;
                    match result {
                        Ok(Ok(())) => {
                            response_idle.as_mut().reset(Instant::now() + self.idle_timeout);
                        }
                        Ok(Err(error)) => {
                            return self
                                .forward_failure(
                                    &mut cache_transaction,
                                    error,
                                    ForwardCacheFailure::Local,
                                )
                                .await;
                        }
                        Err(_) => {
                            return self
                                .forward_failure(
                                    &mut cache_transaction,
                                    RequestFailure::BadGateway,
                                    ForwardCacheFailure::Local,
                                )
                                .await;
                        }
                    }
                }
                result = &mut response => break match result {
                    Ok(response) => response,
                    Err(_) => {
                        return self
                            .forward_failure(
                                &mut cache_transaction,
                                RequestFailure::BadGateway,
                                ForwardCacheFailure::UpstreamProtocol,
                            )
                            .await;
                    }
                },
                () = &mut response_idle, if body_complete => {
                    return self
                        .forward_failure(
                            &mut cache_transaction,
                            RequestFailure::GatewayTimeout,
                            ForwardCacheFailure::UpstreamTimeout,
                        )
                        .await;
                }
                () = tokio::time::sleep_until(lifetime_deadline) => {
                    return self
                        .forward_failure(
                            &mut cache_transaction,
                            RequestFailure::GatewayTimeout,
                            ForwardCacheFailure::UpstreamTimeout,
                        )
                        .await;
                }
                _ = shutdown.changed() => {
                    return self
                        .forward_failure(
                            &mut cache_transaction,
                            RequestFailure::GatewayTimeout,
                            ForwardCacheFailure::Local,
                        )
                        .await;
                }
            }
        };
        let had_uncacheable_framing = upstream_response
            .headers()
            .contains_key(header::TRANSFER_ENCODING)
            || upstream_response.headers().contains_key(header::TRAILER);
        if sanitize_response_headers(upstream_response.headers_mut()).is_err() {
            return self
                .forward_failure(
                    &mut cache_transaction,
                    RequestFailure::BadGateway,
                    ForwardCacheFailure::UpstreamProtocol,
                )
                .await;
        }
        if !body_complete {
            upstream_response
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            drop(body_completion);
        }

        let (parts, body) = upstream_response.into_parts();
        let timed_body = TimedBody::new(body, self.idle_timeout, lifetime_deadline);
        if parts.status.is_server_error()
            && let Some(response) = self
                .forward_stale_response(
                    &mut cache_transaction,
                    ForwardCacheFailure::UpstreamServerStatus,
                )
                .await
        {
            return response;
        }
        if parts.status == StatusCode::NOT_MODIFIED {
            match timeout_at(lifetime_deadline, timed_body.collect()).await {
                Ok(Ok(_)) => {}
                Ok(Err(_)) => {
                    return self
                        .forward_failure(
                            &mut cache_transaction,
                            RequestFailure::BadGateway,
                            ForwardCacheFailure::UpstreamProtocol,
                        )
                        .await;
                }
                Err(_) => {
                    return self
                        .forward_failure(
                            &mut cache_transaction,
                            RequestFailure::GatewayTimeout,
                            ForwardCacheFailure::UpstreamTimeout,
                        )
                        .await;
                }
            }
            if let Some(mut transaction) = cache_transaction.take() {
                if transaction.is_revalidation() {
                    let method = transaction.request().method.clone();
                    let timing = ResponseTiming {
                        request_started: transaction.request().request_started,
                        response_received: transaction.now(),
                        response_received_wall: SystemTime::now(),
                    };
                    let response = transaction
                        .finish_revalidation(&parts.headers, timing)
                        .await;
                    return cached_forward_response(response, &method);
                }
                transaction.complete_without_store();
            }
            return forward_response_from_parts(parts, Bytes::new(), false);
        }

        let mut capture = None;
        if let Some(mut transaction) = cache_transaction.take() {
            if transaction.request().method == Method::GET {
                let headers = parts.headers.clone();
                capture = Some(ForwardCacheCapture {
                    status: parts.status,
                    tags: transaction
                        .surrogate_header()
                        .map_or_else(Vec::new, |header| {
                            response_surrogate_tags_forward(&headers, header)
                        }),
                    timing: ResponseTiming {
                        request_started: transaction.request().request_started,
                        response_received: transaction.now(),
                        response_received_wall: SystemTime::now(),
                    },
                    headers,
                    body: Vec::new(),
                    admissible: !had_uncacheable_framing,
                    transaction: Some(transaction),
                });
            } else {
                transaction.complete_without_store();
            }
        }
        let body = match capture {
            Some(capture) => ForwardCacheBody {
                inner: timed_body,
                capture: Some(capture),
            }
            .boxed(),
            None => timed_body.boxed(),
        };
        Response::from_parts(parts, body)
    }
}
