#[allow(clippy::wildcard_imports)]
use super::*;

async fn write_buffered_response(session: &mut ServerSession, response: ApiResponse) -> bool {
    let (parts, body) = to_http_response(response).into_parts();
    let response_header: ResponseHeader = parts.into();
    if session
        .write_response_header(Box::new(response_header))
        .await
        .is_err()
    {
        return false;
    }
    if body.is_empty() {
        session.finish_body().await.is_ok()
    } else {
        session.write_response_body(body.into(), true).await.is_ok()
    }
}

async fn reply_and_finish(
    mut session: ServerSession,
    response: ApiResponse,
) -> Option<ReusedHttpStream> {
    if write_buffered_response(&mut session, response).await {
        finish_http_session(session).await
    } else {
        None
    }
}

async fn finish_http_session(session: ServerSession) -> Option<ReusedHttpStream> {
    let persistent_settings = HttpPersistentSettings::for_session(&session);
    match session.finish().await {
        Ok(stream) => {
            stream.map(|stream| ReusedHttpStream::from_reusable_stream(stream, persistent_settings))
        }
        Err(_) => None,
    }
}

#[async_trait]
impl ServeHttp for RtmpManagementApi {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        to_http_response(self.api_response(session).await)
    }
}

#[async_trait]
impl HttpServerApp for RtmpManagementHttpApp {
    #[allow(clippy::too_many_lines)]
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        match session.read_request().await {
            Ok(true) => {}
            Ok(false) | Err(_) => return None,
        }
        if *shutdown.borrow() {
            session.set_keepalive(None);
        } else {
            session.set_keepalive(Some(60));
        }

        let (method, path, path_and_query) = request_parts(&session);
        let stream_route =
            management::event_stream_route(&path_and_query, accepts_event_stream(&session));
        if method == "GET" {
            if let Some(stream_route) = stream_route {
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                if let Some(response) =
                    self.api
                        .authentication_error(&method, &path, &path_and_query, &session)
                {
                    if write_buffered_response(
                        &mut session,
                        response.with_correlation(context.correlation_id.clone()),
                    )
                    .await
                    {
                        return finish_http_session(session).await;
                    }
                    return None;
                }
                let last_event_id = match last_event_id(&session) {
                    Ok(last_event_id) => last_event_id,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(context.correlation_id.clone()),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let cursor =
                    match parse_event_stream_cursor(stream_route.query, last_event_id.as_deref()) {
                        Ok(cursor) => cursor,
                        Err(response) => {
                            if write_buffered_response(
                                &mut session,
                                response.with_correlation(context.correlation_id.clone()),
                            )
                            .await
                            {
                                return finish_http_session(session).await;
                            }
                            return None;
                        }
                    };
                self.stream_events(
                    &mut session,
                    stream_route.version,
                    cursor,
                    shutdown,
                    &context,
                )
                .await;
                return finish_http_session(session).await;
            }
            if let Some(route) = vod::match_route(&path) {
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let ticket = match self.api.media_access(
                    &Scope::from(&route),
                    &method,
                    &path,
                    &path_and_query,
                    &session,
                ) {
                    Ok(ticket) => ticket.is_some(),
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(context.correlation_id.clone()),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let response = self
                    .api
                    .vod_response(route, &session)
                    .await
                    .with_correlation(context.correlation_id.clone());
                let response = if ticket {
                    response.with_ticket_privacy()
                } else {
                    response
                };
                RtmpManagementApi::audit_api_operation(&method, &path, &context, &response);
                if !write_buffered_response(&mut session, response).await {
                    return None;
                }
                return finish_http_session(session).await;
            }
            if let Some(recordings::Route::Content(id)) = recordings::match_route(&path) {
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        return reply_and_finish(
                            session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await;
                    }
                };
                let ticketed = match self.api.media_access(
                    &Scope::Recording { id },
                    &method,
                    &path,
                    &path_and_query,
                    &session,
                ) {
                    Ok(ticket) => ticket.is_some(),
                    Err(response) => {
                        return reply_and_finish(
                            session,
                            response.with_correlation(context.correlation_id),
                        )
                        .await;
                    }
                };
                match self.api.recording_content(id, &session).await {
                    recordings::ContentOutcome::Reply(response) => {
                        let response = response.with_correlation(context.correlation_id);
                        let response = if ticketed {
                            response.with_ticket_privacy()
                        } else {
                            response
                        };
                        return reply_and_finish(session, response).await;
                    }
                    recordings::ContentOutcome::Stream(stream) => {
                        if !write_recording_stream(
                            &mut session,
                            stream,
                            &context.correlation_id,
                            ticketed,
                        )
                        .await
                        {
                            return None;
                        }
                        return finish_http_session(session).await;
                    }
                }
            }
            if let Some(route) = media::match_route(&path) {
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let ticket = match self.api.media_access(
                    &Scope::from(&route),
                    &method,
                    &path,
                    &path_and_query,
                    &session,
                ) {
                    Ok(ticket) => ticket,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(context.correlation_id.clone()),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let response = self
                    .api
                    .media_response(route, ticket, &session)
                    .await
                    .with_correlation(context.correlation_id.clone());
                let response = if ticket.is_some() {
                    response.with_ticket_privacy()
                } else {
                    response
                };
                RtmpManagementApi::audit_api_operation(&method, &path, &context, &response);
                if !write_buffered_response(&mut session, response).await {
                    return None;
                }
                return finish_http_session(session).await;
            }
        }

        let response = self.api.api_response(&mut session).await;
        if !write_buffered_response(&mut session, response).await {
            return None;
        }
        finish_http_session(session).await
    }
}
