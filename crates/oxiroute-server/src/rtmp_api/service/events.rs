#[allow(clippy::wildcard_imports)]
use super::*;

impl RtmpManagementHttpApp {
    pub(super) async fn stream_events(
        &self,
        session: &mut ServerSession,
        version: management::EventApiVersion,
        cursor: EventStreamCursor,
        shutdown: &ShutdownWatch,
        context: &AuditContext,
    ) {
        let mut shutdown = shutdown.clone();
        session.set_keepalive(None);
        session.set_write_timeout(Some(EVENT_STREAM_WRITE_TIMEOUT));

        let mut header = ResponseHeader::build(200, None).expect("valid SSE status");
        header
            .insert_header("content-type", "text/event-stream; charset=utf-8")
            .expect("valid SSE content type");
        header
            .insert_header(CACHE_CONTROL, "no-cache, no-store")
            .expect("valid SSE cache policy");
        header
            .insert_header(TRANSFER_ENCODING, "chunked")
            .expect("valid SSE transfer encoding");
        header
            .insert_header("x-content-type-options", "nosniff")
            .expect("valid SSE content protection");
        header
            .insert_header("x-correlation-id", context.correlation_id.as_str())
            .expect("valid SSE correlation ID");
        if session
            .write_response_header(Box::new(header))
            .await
            .is_err()
        {
            return;
        }

        let limit = cursor.limit;
        let mut cursor = cursor
            .after
            .unwrap_or_else(crate::operational_event::current_cursor);
        if !self.write_sse_frame(session, ready_frame(cursor)).await {
            return;
        }

        let start = Instant::now() + EVENT_STREAM_HEARTBEAT;
        let mut heartbeat = interval_at(start, EVENT_STREAM_HEARTBEAT);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            if *shutdown.borrow() {
                let _ = self.write_sse_frame(session, shutdown_frame()).await;
                return;
            }

            let notification = crate::operational_event::wait_for_event();
            let page = crate::operational_event::page(cursor, limit);
            if page.cursor_lost {
                let _ = self
                    .write_sse_frame(
                        session,
                        resync_frame(cursor, page.oldest_cursor, page.latest_cursor),
                    )
                    .await;
                return;
            }
            if !page.events.is_empty() {
                for event in page.events {
                    if !self
                        .write_sse_frame(
                            session,
                            match version {
                                management::EventApiVersion::V1 => operational_frame(&event),
                                management::EventApiVersion::V2 => operational_frame_v2(&event),
                            },
                        )
                        .await
                    {
                        return;
                    }
                    cursor = event.cursor;
                }
                continue;
            }

            tokio::select! {
                biased;
                () = notification => {}
                _ = heartbeat.tick() => {
                    if !self.write_sse_frame(session, heartbeat_frame()).await {
                        return;
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        let _ = self.write_sse_frame(session, shutdown_frame()).await;
                        return;
                    }
                }
            }
        }
    }

    async fn write_sse_frame(&self, session: &mut ServerSession, frame: Vec<u8>) -> bool {
        if frame.len() > EVENT_STREAM_FRAME_LIMIT {
            return false;
        }
        session
            .write_response_body(Bytes::from(frame), false)
            .await
            .is_ok()
    }
}

#[derive(Clone, Copy)]
pub(super) struct EventStreamCursor {
    pub(super) after: Option<u64>,
    pub(super) limit: usize,
}

pub(super) fn parse_event_stream_cursor(
    query: Option<&str>,
    last_event_id: Option<&str>,
) -> Result<EventStreamCursor, ApiResponse> {
    let mut after = None;
    let mut limit = EVENT_STREAM_BATCH_LIMIT;
    if let Some(query) = query {
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name {
                "after" if after.is_none() => {
                    after = Some(value.parse().map_err(|_| {
                        ApiResponse::error(400, "invalid_cursor", "event cursor is invalid")
                    })?);
                }
                "after" => {
                    return Err(ApiResponse::error(
                        400,
                        "invalid_cursor",
                        "event cursor must be specified once",
                    ));
                }
                "limit" if (1..=EVENT_STREAM_BATCH_LIMIT).contains(&value.parse().unwrap_or(0)) => {
                    limit = value.parse().expect("validated event stream limit");
                }
                "limit" => {
                    return Err(ApiResponse::error(
                        400,
                        "invalid_limit",
                        "event stream limit must be between 1 and 64",
                    ));
                }
                _ => {
                    return Err(ApiResponse::error(
                        400,
                        "invalid_query",
                        "event stream query parameter is invalid",
                    ));
                }
            }
        }
    }

    if let Some(last_event_id) = last_event_id {
        after =
            Some(last_event_id.parse().map_err(|_| {
                ApiResponse::error(400, "invalid_cursor", "Last-Event-ID is invalid")
            })?);
    }
    Ok(EventStreamCursor { after, limit })
}

pub(super) fn last_event_id(session: &ServerSession) -> Result<Option<String>, ApiResponse> {
    let mut ids = session.req_header().headers.get_all(&LAST_EVENT_ID).iter();
    match (ids.next(), ids.next()) {
        (None, _) => Ok(None),
        (Some(_), Some(_)) => Err(ApiResponse::error(
            400,
            "invalid_cursor",
            "Last-Event-ID must be specified once",
        )),
        (Some(value), None) => value
            .to_str()
            .map(str::to_owned)
            .map(Some)
            .map_err(|_| ApiResponse::error(400, "invalid_cursor", "Last-Event-ID is invalid")),
    }
}

pub(super) fn ready_frame(cursor: u64) -> Vec<u8> {
    let data = serde_json::to_string(&super::super::dto::SseReadyDto::new(cursor))
        .expect("SSE ready DTO serializes");
    format!("event: ready\ndata: {data}\n\n").into_bytes()
}

pub(super) fn operational_frame(event: &crate::operational_event::OperationalEvent) -> Vec<u8> {
    let data = crate::operational_event::v1_event_value(event).to_string();
    let mut frame = String::new();
    let _ = writeln!(frame, "id: {}", event.cursor);
    let _ = writeln!(
        frame,
        "event: {}",
        crate::operational_event::v1_sse_event_name(event.event)
    );
    let _ = writeln!(frame, "data: {data}\n");
    frame.into_bytes()
}

pub(super) fn operational_frame_v2(event: &crate::operational_event::OperationalEvent) -> Vec<u8> {
    let data = crate::operational_event::v2_event_value(event).to_string();
    let mut frame = String::new();
    let _ = writeln!(frame, "id: {}", event.cursor);
    let _ = writeln!(frame, "event: {}", event.event.as_str());
    let _ = writeln!(frame, "data: {data}\n");
    frame.into_bytes()
}

pub(super) fn resync_frame(requested: u64, oldest: Option<u64>, latest: u64) -> Vec<u8> {
    let data = serde_json::to_string(&super::super::dto::SseResyncDto::new(
        requested, oldest, latest,
    ))
    .expect("SSE resync DTO serializes");
    format!("event: resync_required\ndata: {data}\n\n").into_bytes()
}

pub(super) fn heartbeat_frame() -> Vec<u8> {
    b": heartbeat\n\n".to_vec()
}

pub(super) fn shutdown_frame() -> Vec<u8> {
    let data = serde_json::to_string(&super::super::dto::SseShutdownDto::server_shutdown())
        .expect("SSE shutdown DTO serializes");
    format!("event: shutdown\ndata: {data}\n\n").into_bytes()
}

pub(super) fn accepts_event_stream(session: &ServerSession) -> bool {
    session
        .req_header()
        .headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| {
            value.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("text/event-stream")
            })
        })
}
