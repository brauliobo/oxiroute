#[allow(clippy::wildcard_imports)]
use super::*;

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
            evacuation_pin: None,
            evacuations: 0,
            evacuated_from: Vec::new(),
            retry_server: None,
            queue_tag: QueueTag::default(),
            retry_delay_pending: false,
            prior_attempts: 0,
            rescued: false,
            retry_prepaid: false,
            response_status_override: None,
            response_header_overrides: Vec::new(),
            started_at,
            request_facts: RequestFacts::default(),
            now: started_at,
            total_deadline: started_at + self.service.upstream_io_timeout(),
            client_deadline: None,
            deadline: started_at + self.service.upstream_io_timeout(),
            operation_result: None,
            websocket_reference: None,
            generation: self.generation.clone(),
            cache_transaction: None,
            cache_capture: None,
            response_buffer: None,
            cache_response_handled: false,
            trace: AttemptTrace::default(),
            inflight: None,
            request_id: None,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        let hop_by_hop = strip_hop_by_hop(session);
        ctx.request_id = Some(stamp_request(session.req_header_mut_keeping_facts())?);
        ctx.request_facts = session.request_facts();
        ctx.adopt_downstream_deadline(session);
        ctx.register_inflight(session, self.service.access_log());
        let arrived = ctx.started_at;
        let remaining = ctx.remaining_at(arrived, false)?;
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
        let (authority, content_length, upgrade) = {
            let request = session.req_header();
            (
                request_authority(request),
                request_content_length(session, ctx.request_facts),
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
                respond_status(session, 503).await?;
                ctx.allow_downstream_drain(session);
                return Ok(true);
            };
            ctx.websocket_reference = Some(reference);
        }

        let (Ok(authority), Ok(())) = (authority, hop_by_hop) else {
            respond_status(session, 400).await?;
            return Ok(true);
        };
        let request = session.req_header();
        if content_length.is_err() || !is_unambiguous_http_path(request.uri.path()) {
            respond_status(session, 400).await?;
            return Ok(true);
        }
        let Some(route) =
            self.service
                .select_route(authority.as_ref(), &request.uri, &request.method)
        else {
            respond_status_with_body(session, 404, Bytes::from_static(b"route not found\n"))
                .await?;
            ctx.allow_downstream_drain(session);
            return Ok(true);
        };
        ctx.authority = authority;
        if let Some(inflight) = &ctx.inflight {
            inflight.place_route(route.route_id.shared());
        }
        ctx.route = Some(Arc::clone(&route));
        session.set_total_drain_timeout(Some(
            ctx.remaining_at(arrived, false)?
                .min(route.policy.read_timeout),
        ));
        if content_length
            .expect("checked content length")
            .is_some_and(|length| route.policy.exceeds_body_limit(length))
        {
            respond_status(session, 413).await?;
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
        if route.access.is_some() {
            ctx.refresh_clock();
        }

        match pingora_request_header_decisions(session, &route, ctx.authority.as_ref()) {
            Ok(decisions) => ctx.request_header_decisions = decisions,
            Err(RequestPolicyError::SourceTooLarge) => {
                respond_status(session, 431).await?;
                ctx.allow_downstream_drain(session);
                return Ok(true);
            }
            Err(error) => return Err(pingora_request_policy_error(error).into()),
        }
        if let HttpActionPlan::Proxy(proxy) = &route.action
            && let Some(cache) = &proxy.policy.cache
        {
            let (method, uri) = {
                let request = session.req_header();
                (request.method.clone(), request.uri.clone())
            };
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
            ctx.refresh_clock();
        }
        let response_sent = execute_route_action(&self.service, session, ctx, route).await?;
        if response_sent {
            ctx.allow_downstream_drain(session);
        }
        Ok(response_sent)
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        if ctx.has_attempted() {
            ctx.refresh_clock();
        }
        let mut now = ctx.now;
        let remaining = ctx.remaining_at(now, true)?;
        let Some(pool) = &ctx.pool else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        if ctx.selected.is_none() {
            if ctx.retry_delay_pending {
                let delay = proxy_policy(ctx).next_retry_delay();
                if delay > remaining {
                    return Err(Error::new_up(ErrorType::ReadTimedout));
                }
                ctx.enter_phase(Phase::RetryWait, now);
                tokio::time::sleep(delay).await;
                ctx.retry_delay_pending = false;
                ctx.now = Instant::now();
                now = ctx.now;
            }
            ctx.enter_phase(Phase::Queued, now);
            let selection_started = now;
            let (selected, waited) = if let Some(server) = ctx.retry_server.take() {
                (
                    pool.select_server_endpoint(&server, ctx.queue_tag).await?,
                    true,
                )
            } else if let Some(server) = affinity_server(session, ctx) {
                (
                    pool.select_server_endpoint(&server, ctx.queue_tag).await?,
                    true,
                )
            } else {
                let wait = ctx.remaining_at(now, true)?;
                let (selected, waited) = promptly(
                    wait,
                    pool.select_endpoint(&ctx.attempted_upstreams, ctx.queue_tag),
                )
                .await
                .map_err(|_| Error::new_up(ErrorType::ReadTimedout))?;
                (selected?, waited)
            };
            if waited {
                ctx.now = Instant::now();
                now = ctx.now;
            }
            ctx.trace.queue_wait += now.saturating_duration_since(selection_started);
            ctx.trace.attempt_started = Some(now);
            let server: Arc<str> = selected.server_name().into();
            ctx.trace.servers.push(Arc::clone(&server));
            ctx.trace.stats = Some(Arc::clone(selected.server_stats()));
            if let Some(inflight) = &ctx.inflight {
                inflight.place_pool(pool.label().shared());
                inflight.place_server(server);
                selected.observe_queue_position(inflight.queue_cell());
                inflight.set_phase(Phase::Connecting, now);
            }
            ctx.selected_upstream_host = selected_upstream_host_for(
                session.req_header(),
                selected.endpoint(),
                &proxy_policy(ctx).upstream_host,
                ctx.authority.as_ref(),
            )?;
            ctx.attempted_upstreams
                .push(selected.server_name().to_owned());
            ctx.selected_observation = Some(selected.observation());
            ctx.selected = Some(selected);
        }
        let remaining = ctx.remaining_at(now, true)?;
        let route_policy = proxy_route(ctx).policy;
        let timeouts = proxy_policy(ctx).timeouts;
        let attempt = |explicit: Option<u64>, inherited: Duration| {
            explicit
                .map_or(inherited, Duration::from_millis)
                .min(remaining)
        };
        let response = pool.server_timeout(route_policy.read_timeout);
        let mut peer = ctx
            .selected
            .as_mut()
            .expect("selected endpoint initialized")
            .prepare_peer_with_timeouts(
                pool,
                now,
                attempt(
                    timeouts.connect_ms,
                    pool.connect_timeout(route_policy.connect_timeout),
                ),
                attempt(timeouts.header_ms, response),
                pool.server_timeout(route_policy.write_timeout)
                    .min(remaining),
            )
            .await?;
        peer.options.body_read_timeout = Some(attempt(timeouts.idle_ms, response));
        Ok(Box::new(peer))
    }

    fn upstream_attempt_cancelled(
        &self,
        session: &Session,
        ctx: &Self::CTX,
    ) -> Option<UpstreamCancellation> {
        ctx.arm_evacuation(session)
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut error: Box<Error>,
    ) -> Box<Error> {
        if let Some(cancelled) = ctx.cancelled_by_operator() {
            return cancelled;
        }
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
        let has_budget = ctx.attempt_count() <= usize::from(policy.max_retries);
        let retry_target = retry_target_for(ctx, false);
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => ctx.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => ctx
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
        };
        let trigger = connect_retry_trigger(&error);
        let route_retry_allowed = !has_address_fallback
            && ctx.connection_retryable
            && trigger.is_some_and(|trigger| policy.retries_on(trigger))
            && has_budget
            && target_available
            && ctx.remaining(true).is_ok()
            && ctx.spend_retry_budget();
        let retry = should_retry_connection(has_address_fallback, route_retry_allowed);
        error.set_retry(retry);
        if !has_address_fallback {
            ctx.settle_attempt(&error, retry);
        }
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
        let prepaid = std::mem::take(&mut ctx.retry_prepaid);
        if let Some(cancelled) = ctx.cancelled_by_operator() {
            return cancelled;
        }
        let rescue_only = is_rescue_only(&error, proxy_policy(ctx));
        if let Some(failure) = passive_failure_for_error(&error).filter(|_| !rescue_only) {
            ctx.record_passive_failure(failure);
        }
        ctx.release_lease();
        let policy = proxy_policy(ctx);
        let refused = is_pool_unavailable(&error);
        let has_budget = ctx.attempt_count() <= usize::from(policy.max_retries);
        let retry_target = retry_target_for(ctx, refused);
        let target_available = match retry_target {
            HttpRetryTarget::SameServer => ctx.attempted_upstreams.last().is_some(),
            HttpRetryTarget::NextServer => ctx
                .pool
                .as_ref()
                .is_some_and(|pool| pool.has_unattempted(&ctx.attempted_upstreams)),
        };
        let eligible = if refused {
            ctx.connection_retryable
        } else {
            ctx.replay_retryable && retryable_upstream_error(&error, session, policy)
        };
        let retry = eligible
            && request_body_replayable(session)
            && response_is_retryable(session)
            && has_budget
            && target_available
            && ctx.remaining(true).is_ok()
            && (prepaid || ctx.spend_retry_budget());
        error.set_retry(retry);
        ctx.settle_attempt(&error, retry);
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
        ctx.trace.failure_reason = Some(failure_reason(error, ctx, session));
        if ctx.client_deadline_expired() {
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
        let reason = error_reason(error, ctx, session);
        let code = reason.map_or_else(|| proxy_error_status(error), ErrorReason::status);
        if let Some(reason) = reason {
            debug!("request ended by {}", reason.as_str());
        }
        let nginx_server = ctx.route.as_ref().and_then(|route| match &route.action {
            HttpActionPlan::Proxy(proxy) => proxy.policy.nginx_error_server.clone(),
            HttpActionPlan::Fixed(_) | HttpActionPlan::Redirect(_) | HttpActionPlan::Static(_) => {
                None
            }
        });
        if matches!(code, 502 | 504)
            && error.esource() == &ErrorSource::Upstream
            && let Some(server) = nginx_server
        {
            let mut headers = vec![
                (SERVER, server.clone()),
                (CONTENT_TYPE, HeaderValue::from_static("text/html")),
            ];
            headers.extend(reason.map(ErrorReason::header));
            let phrase = if code == 504 {
                "Gateway Time-out"
            } else {
                "Bad Gateway"
            };
            let body = nginx_error_body(code, phrase, &server);
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
            && let Err(write_error) =
                respond_error(session, code, error, reason, ctx.pool.as_deref()).await
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
        let now = Instant::now();
        ctx.observe(session, now);
        if session.was_upgraded() {
            return Ok(());
        }
        if session.retry_buffer_truncated() {
            ctx.pin_evacuation(LeftRunning::BodyNotReplayable);
        }
        let remaining = ctx.remaining_at(now, false)?;
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
        session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        ctx.remaining_cached(true)?;
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
            let mut uri = upstream_request.uri.clone();
            rewrite_upstream_path(&mut uri, rewrite)?;
            upstream_request.set_uri(uri);
        }
        if ctx
            .cache_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.request().method == Method::HEAD)
        {
            upstream_request.set_method(Method::GET);
        }
        if let Some(transaction) = ctx.cache_transaction.as_ref() {
            let mut validators = http::HeaderMap::new();
            transaction.apply_validators(&mut validators);
            for (name, value) in &validators {
                upstream_request.set_header(name.clone(), value.clone())?;
            }
        }
        if let Some(host) = &ctx.selected_upstream_host {
            upstream_request.set_header(HOST, host.clone())?;
        } else {
            upstream_request.remove_header(&HOST);
        }
        remove_downstream_close(upstream_request)?;
        if session.is_http2() && upstream_request.version != http::Version::HTTP_2 {
            upstream_request.remove_header(&TE);
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
        ctx.remaining_cached(true)?;
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
        let now = ctx.refresh_clock();
        let remaining = ctx.remaining_at(now, true)?;
        let write_timeout = session
            .get_write_timeout()
            .map_or(remaining, |timeout| timeout.min(remaining));
        session.set_write_timeout(Some(write_timeout));
        ctx.close_when_retired(session);
        let refused = response.status == StatusCode::SERVICE_UNAVAILABLE
            && response.headers.contains_key(POOL_UNAVAILABLE_HEADER);
        let status = response.status.as_u16();
        let rescued = !refused && proxy_policy(ctx).rescues_status(status);
        let retries_on_status = refused || rescued || proxy_policy(ctx).retries_on_status(status);
        if retries_on_status
            && response_status_retryable(session, ctx, refused, rescued)
            && ctx.spend_retry_budget()
        {
            ctx.retry_prepaid = true;
            let outcome = if rescued {
                AttemptOutcome::Rescued
            } else {
                AttemptOutcome::StatusRetry
            };
            ctx.trace.finish(outcome, Some(status));
            if rescued {
                ctx.begin_rescue(status);
            }
            let mut error = if refused {
                Error::new_up(POOL_UNAVAILABLE)
            } else {
                Error::new_up(ErrorType::HTTPStatus(response.status.as_u16()))
            };
            error.set_retry(true);
            return Err(error);
        }
        ctx.commit_response(response.status)?;
        if !retries_on_status {
            ctx.record_passive_success();
        } else if !rescued {
            ctx.record_passive_failure(HealthFailure::UnexpectedStatus);
        }
        ctx.note_response(status, now);
        ctx.enter_phase(Phase::Streaming, now);
        // The framing headers are read before the policy strips them, and only a buffered or cached response cares.
        let had_uncacheable_framing = (proxy_route(ctx)
            .policy
            .response_body_buffer_limit()
            .is_some()
            || ctx.cache_transaction.is_some())
            && (response.headers.contains_key(TRANSFER_ENCODING)
                || response.headers.contains_key(TRAILER));
        let remove_hop_by_hop = response.status != http::StatusCode::SWITCHING_PROTOCOLS;
        if let Some(status) = ctx.response_status_override {
            response.set_status(status)?;
        }
        for (name, value) in &ctx.response_header_overrides {
            response.add_header(name.clone(), value.clone())?;
        }
        apply_response_policy(response, proxy_policy(ctx), remove_hop_by_hop)?;
        apply_affinity_cookie(session, ctx, response)?;
        ctx.stamp_queue_headers(response)?;
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
        // After the capture: a cached response replays the origin's headers, not this request's id.
        if let Some(id) = &ctx.request_id {
            stamp_response(response, id)?;
        }
        Ok(())
    }

    async fn connected_to_upstream(
        &self,
        _session: &mut Session,
        reused: bool,
        _peer: &HttpPeer,
        #[cfg(unix)] _fd: std::os::unix::io::RawFd,
        #[cfg(windows)] _socket: std::os::windows::io::RawSocket,
        digest: Option<&Digest>,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        let queued = ctx
            .selected
            .as_ref()
            .is_some_and(|selected| selected.queue_position() > 0);
        let now = if reused && !queued {
            ctx.now
        } else {
            ctx.refresh_clock()
        };
        ctx.remaining_at(now, true)?;
        ctx.enter_phase(Phase::AwaitingHeaders, now);
        ctx.trace.connected_at = Some(now);
        ctx.trace.connect = ctx
            .trace
            .attempt_started
            .map(|started| now.saturating_duration_since(started));
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
        let now = Instant::now();
        if session.was_upgraded() {
            ctx.observe(session, now);
            return Ok(None);
        }
        let remaining = ctx.remaining_at(now, false)?;
        let write_timeout = session
            .get_write_timeout()
            .map_or(remaining, |timeout| timeout.min(remaining));
        session.set_write_timeout(Some(write_timeout));
        ctx.observe(session, now);
        Ok(None)
    }

    fn upstream_response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Option<Duration>> {
        if !session.was_upgraded() && !end_of_stream {
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
        let now = ctx.refresh_clock();
        let elapsed = now.saturating_duration_since(ctx.started_at);
        if ctx.operation_result.is_none() {
            let result = classify_http_result(
                (!ctx.cache_response_handled).then_some(error).flatten(),
                response_status,
            );
            ctx.operation_result = Some(result);
            if let Err(metric_error) = ctx.listener.record_http_operation(result, elapsed) {
                warn!("could not account for HTTP operation metrics: {metric_error}");
            }
        }
        ctx.observe(session, now);
        ctx.release_lease();
        ctx.account_final_attempt(session, now);
        ctx.record_failed_evacuations(error);
        if let Some(access_log) = self.service.access_log() {
            let record = access_record::build(
                ctx,
                session,
                response_status,
                elapsed,
                access_log.logged_service(),
            );
            if let Err(error) = access_log.write_with(|line| {
                record.write_json(line);
                Ok(())
            }) {
                warn!("HTTP access log write failed: {error}");
            }
        }
    }
}
