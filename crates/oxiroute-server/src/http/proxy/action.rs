#[allow(clippy::wildcard_imports)]
use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "local actions and bounded internal redirects share one response state machine"
)]
pub(super) async fn execute_route_action(
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
    let mut method = method.clone();
    loop {
        ctx.route = Some(Arc::clone(&route));
        match &route.action {
            HttpActionPlan::Proxy(proxy) => {
                ctx.response_status_override = status_override;
                ctx.response_header_overrides = std::mem::take(&mut status_headers);
                let upgrade = session.is_upgrade_req();
                ctx.connection_retryable = proxy.policy.max_retries > 0 && !upgrade;
                let body_is_empty = session.is_body_empty();
                let replay_safe = proxy.policy.request_is_replay_safe(
                    &method,
                    &session.req_header().headers,
                    body_is_empty,
                    route.policy.request_buffering,
                );
                ctx.replay_retryable = proxy.policy.max_retries > 0 && replay_safe && !upgrade;
                ctx.evacuation_pin = LeftRunning::unmovable(upgrade, replay_safe);
                ctx.adopt_route_total(proxy.policy.timeouts.total_ms.map(Duration::from_millis));
                if let Some(budget) = &proxy.policy.retry_budget {
                    budget.record_request(Instant::now());
                }
                let pool = match &proxy.select {
                    Some(select) => {
                        let Ok(pool) = select.choose(&session.req_header().headers, &proxy.pool)
                        else {
                            session.respond_error(400).await?;
                            return Ok(true);
                        };
                        ctx.listener
                            .record_pool_selection(&route.route_id, pool.selector().name());
                        pool
                    }
                    None => &proxy.pool,
                };
                if let Some(inflight) = &ctx.inflight {
                    inflight.set_pool(pool.selector().name());
                }
                ctx.queue_tag = pool.selector().queue_tag(
                    proxy.policy.queue_class.as_deref(),
                    &session.req_header().headers,
                );
                ctx.pool = Some(Arc::clone(pool));
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
                    method == Method::HEAD,
                )
                .await?;
                return Ok(true);
            }
            HttpActionPlan::Redirect(redirect) => {
                let Some(location) =
                    redirect_location(&redirect.location, session, ctx.authority.as_ref(), &uri)
                else {
                    respond_status(session, 400).await?;
                    return Ok(true);
                };
                let mut headers = redirect.headers.to_vec();
                headers.append(&mut status_headers);
                headers.push((LOCATION, location));
                let body = nginx_server_marker(&headers).map_or_else(Bytes::new, |server| {
                    nginx_error_body(
                        redirect.status,
                        http::StatusCode::from_u16(redirect.status)
                            .ok()
                            .and_then(|status| status.canonical_reason())
                            .unwrap_or("Redirect"),
                        &server,
                    )
                });
                write_local_response(
                    session,
                    redirect.status,
                    &headers,
                    body,
                    method == Method::HEAD,
                )
                .await?;
                return Ok(true);
            }
            HttpActionPlan::Static(files) => {
                let mut internal_redirect: Option<(String, Vec<(HeaderName, HeaderValue)>)> = None;
                if method != Method::GET && method != Method::HEAD {
                    internal_redirect = write_static_error(
                        session,
                        files,
                        405,
                        false,
                        &[(ALLOW, HeaderValue::from_static("GET, HEAD"))],
                    )
                    .await?;
                    status_override = Some(405);
                    status_headers.clear();
                } else {
                    let result = files.serve(uri.path()).await;
                    match result {
                        Ok(StaticTarget::File(file)) => {
                            if let Some(status) = status_override.take() {
                                write_static_file_with_status(
                                    session,
                                    files,
                                    file,
                                    status,
                                    method == Method::HEAD,
                                    None,
                                    &status_headers,
                                )
                                .await?;
                                status_headers.clear();
                            } else if let Some(headers) =
                                write_static_file(session, files, file, method == Method::HEAD)
                                    .await?
                            {
                                status_headers = headers;
                                internal_redirect = write_static_error(
                                    session,
                                    files,
                                    416,
                                    method == Method::HEAD,
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
                                method == Method::HEAD,
                            )
                            .await?;
                        }
                        Ok(StaticTarget::Status(status)) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                status,
                                method == Method::HEAD,
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
                                method == Method::HEAD,
                            )
                            .await?;
                        }
                        Ok(StaticTarget::InternalRedirect { path }) => {
                            internal_redirect = Some((path, Vec::new()));
                        }
                        Err(StaticServeError::Unsafe) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                403,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(403);
                            status_headers.clear();
                        }
                        Err(StaticServeError::NotFound) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                404,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(404);
                            status_headers.clear();
                        }
                        Err(StaticServeError::TooLarge | StaticServeError::Unavailable) => {
                            internal_redirect = write_static_error(
                                session,
                                files,
                                500,
                                method == Method::HEAD,
                                &[],
                            )
                            .await?;
                            status_override = Some(500);
                            status_headers.clear();
                        }
                    }
                }
                let Some((path, error_headers)) = internal_redirect else {
                    return Ok(true);
                };
                status_headers.extend(error_headers);
                if internal_redirects >= MAX_INTERNAL_REDIRECTS {
                    write_local_response(
                        session,
                        500,
                        &files.headers(500),
                        Bytes::new(),
                        method == Method::HEAD,
                    )
                    .await?;
                    return Ok(true);
                }
                internal_redirects += 1;
                if method != Method::GET && method != Method::HEAD {
                    method = Method::GET;
                }
                uri = internal_uri(&path, &uri)?;
                let request = session.req_header_mut();
                request.method = method.clone();
                request.uri = uri.clone();
                let Some(next) = service.select_route(ctx.authority.as_ref(), &uri, &method) else {
                    respond_status(session, 404).await?;
                    return Ok(true);
                };
                if let Some(access) = &next.access
                    && !access.authorizes(&session.req_header().headers).await
                {
                    write_local_response(
                        session,
                        401,
                        &[(WWW_AUTHENTICATE, access.challenge().clone())],
                        Bytes::new(),
                        method == Method::HEAD,
                    )
                    .await?;
                    return Ok(true);
                }
                route = next;
            }
        }
    }
}

pub(super) async fn write_local_response(
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

pub(super) fn redirect_location(
    location: &RedirectLocationPlan,
    session: &Session,
    authority: Option<&Authority>,
    uri: &http::Uri,
) -> Option<HeaderValue> {
    let scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let host = authority.map(normalized_redirect_host);
    let request_uri = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    expand_redirect_location(
        location,
        RedirectContext {
            scheme,
            normalized_host: host.as_deref(),
            request_uri,
        },
    )
}
