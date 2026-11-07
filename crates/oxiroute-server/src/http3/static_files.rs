#[allow(clippy::wildcard_imports)]
use super::*;

const MAX_STATIC_REDIRECTS: usize = 10;

enum H3StaticErrorAction {
    File {
        file: StaticFile,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    InternalRedirect {
        path: String,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    Literal {
        body: Bytes,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    Empty,
}

async fn h3_static_error_action(
    files: &crate::http::action::StaticFilesPlan,
    status: u16,
) -> H3StaticErrorAction {
    match files.error_document(status).await {
        Some(StaticErrorTarget::File { file, headers }) => {
            H3StaticErrorAction::File { file, headers }
        }
        Some(StaticErrorTarget::InternalRedirect { path, headers }) => {
            H3StaticErrorAction::InternalRedirect { path, headers }
        }
        Some(StaticErrorTarget::Literal { body, headers }) => {
            H3StaticErrorAction::Literal { body, headers }
        }
        None => H3StaticErrorAction::Empty,
    }
}

#[allow(clippy::too_many_lines)]
pub(super) async fn send_h3_static_request<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    files: &crate::http::action::StaticFilesPlan,
    request: &Request<()>,
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        let headers = [(
            HeaderName::from_static("allow"),
            HeaderValue::from_static("GET, HEAD"),
        )];
        return send_h3_response(
            stream,
            StatusCode::METHOD_NOT_ALLOWED,
            &headers,
            Bytes::new(),
            false,
            deadline,
        )
        .await
        .then_some(StatusCode::METHOD_NOT_ALLOWED.as_u16());
    }

    let mut path = request.uri().path().to_owned();
    let mut status_override = None;
    let mut pending_headers = Vec::new();
    for _ in 0..=MAX_STATIC_REDIRECTS {
        match files.serve(&path).await {
            Ok(StaticTarget::File(file)) => {
                let status = status_override.unwrap_or(200);
                let range = if status == 200 {
                    match files.request_decision(request.headers(), &file) {
                        StaticRequestDecision::NotModified => {
                            let mut headers = files.headers(304);
                            headers.extend(h3_static_validator_headers(files, &file));
                            return send_h3_response(
                                stream,
                                StatusCode::NOT_MODIFIED,
                                &headers,
                                Bytes::new(),
                                true,
                                deadline,
                            )
                            .await
                            .then_some(StatusCode::NOT_MODIFIED.as_u16());
                        }
                        StaticRequestDecision::PreconditionFailed => {
                            let mut headers = files.headers(412);
                            headers.extend(h3_static_validator_headers(files, &file));
                            return send_h3_response(
                                stream,
                                StatusCode::PRECONDITION_FAILED,
                                &headers,
                                Bytes::new(),
                                head,
                                deadline,
                            )
                            .await
                            .then_some(StatusCode::PRECONDITION_FAILED.as_u16());
                        }
                        StaticRequestDecision::RangeNotSatisfiable => {
                            let headers = [
                                (
                                    HeaderName::from_static("accept-ranges"),
                                    HeaderValue::from_static("bytes"),
                                ),
                                (
                                    HeaderName::from_static("content-range"),
                                    HeaderValue::from_str(&format!("bytes */{}", file.size))
                                        .expect("static size is a valid header value"),
                                ),
                            ];
                            let _ = send_h3_response(
                                stream,
                                StatusCode::RANGE_NOT_SATISFIABLE,
                                &headers,
                                Bytes::new(),
                                head,
                                deadline,
                            )
                            .await;
                            return Some(StatusCode::RANGE_NOT_SATISFIABLE.as_u16());
                        }
                        StaticRequestDecision::Serve { range } => range,
                    }
                } else {
                    None
                };
                return send_h3_static_file(
                    stream,
                    files,
                    file,
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    range,
                    head,
                    &pending_headers,
                    deadline,
                )
                .await;
            }
            Ok(StaticTarget::Autoindex { body }) => {
                let status = status_override.take().unwrap_or(200);
                let mut headers = files.headers(status);
                headers.extend(std::mem::take(&mut pending_headers));
                headers.push((
                    HeaderName::from_static("content-type"),
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ));
                return send_h3_response(
                    stream,
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    &headers,
                    body,
                    head,
                    deadline,
                )
                .await
                .then_some(status);
            }
            Ok(StaticTarget::DirectoryRedirect { path: redirect }) => {
                let Ok(location) = HeaderValue::from_str(&redirect) else {
                    return send_h3_error(
                        stream,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        b"static directory redirect is invalid\n",
                        deadline,
                    )
                    .await
                    .then_some(StatusCode::INTERNAL_SERVER_ERROR.as_u16());
                };
                let mut headers = files.headers(301);
                headers.extend(std::mem::take(&mut pending_headers));
                headers.push((HeaderName::from_static("location"), location));
                return send_h3_response(
                    stream,
                    StatusCode::MOVED_PERMANENTLY,
                    &headers,
                    Bytes::new(),
                    head,
                    deadline,
                )
                .await
                .then_some(StatusCode::MOVED_PERMANENTLY.as_u16());
            }
            Ok(StaticTarget::InternalRedirect { path: redirect }) => {
                path = redirect;
            }
            Ok(StaticTarget::Status(status)) => {
                if let Some(next) = apply_h3_static_error(
                    stream,
                    files,
                    status,
                    head,
                    &mut path,
                    &mut status_override,
                    &mut pending_headers,
                    deadline,
                )
                .await
                {
                    return Some(next);
                }
            }
            Err(error) => {
                let status = match error {
                    StaticServeError::Unsafe => 403,
                    StaticServeError::NotFound => 404,
                    StaticServeError::TooLarge | StaticServeError::Unavailable => 500,
                };
                if let Some(next) = apply_h3_static_error(
                    stream,
                    files,
                    status,
                    head,
                    &mut path,
                    &mut status_override,
                    &mut pending_headers,
                    deadline,
                )
                .await
                {
                    return Some(next);
                }
            }
        }
    }
    send_h3_error(
        stream,
        StatusCode::INTERNAL_SERVER_ERROR,
        b"static internal redirect limit exceeded\n",
        deadline,
    )
    .await
    .then_some(StatusCode::INTERNAL_SERVER_ERROR.as_u16())
}

#[allow(clippy::too_many_arguments)]
async fn apply_h3_static_error<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    files: &crate::http::action::StaticFilesPlan,
    status: u16,
    head: bool,
    path: &mut String,
    status_override: &mut Option<u16>,
    pending_headers: &mut Vec<(HeaderName, HeaderValue)>,
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    match h3_static_error_action(files, status).await {
        H3StaticErrorAction::File { file, headers } => {
            let mut all_headers = headers.into_vec();
            all_headers.extend(std::mem::take(pending_headers));
            send_h3_static_file(
                stream,
                files,
                file,
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                None,
                head,
                &all_headers,
                deadline,
            )
            .await
        }
        H3StaticErrorAction::InternalRedirect {
            path: redirect,
            headers,
        } => {
            *path = redirect;
            *status_override = Some(status);
            pending_headers.extend(headers.into_vec());
            None
        }
        H3StaticErrorAction::Literal { body, headers } => {
            let mut all_headers = files.headers(status);
            all_headers.extend(headers.into_vec());
            all_headers.extend(std::mem::take(pending_headers));
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let _ = send_h3_response(stream, status, &all_headers, body, head, deadline).await;
            Some(status.as_u16())
        }
        H3StaticErrorAction::Empty => {
            let mut all_headers = files.headers(status);
            all_headers.extend(std::mem::take(pending_headers));
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let _ =
                send_h3_response(stream, status, &all_headers, Bytes::new(), head, deadline).await;
            Some(status.as_u16())
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn send_h3_static_file<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    files: &crate::http::action::StaticFilesPlan,
    file: StaticFile,
    status: StatusCode,
    range: Option<(u64, u64)>,
    head: bool,
    extra_headers: &[(HeaderName, HeaderValue)],
    deadline: Instant,
) -> Option<u16>
where
    S: h3::quic::BidiStream<Bytes> + Send,
{
    let status = if range.is_some() && status == StatusCode::OK {
        StatusCode::PARTIAL_CONTENT
    } else {
        status
    };
    let (start, end) = range.unwrap_or_else(|| (0, file.size.saturating_sub(1)));
    let length = if file.size == 0 { 0 } else { end - start + 1 };
    if length > H3_MAX_RESPONSE_BODY_BYTES {
        let _ = send_h3_error(
            stream,
            StatusCode::PAYLOAD_TOO_LARGE,
            b"static response body exceeds the HTTP/3 limit\n",
            deadline,
        )
        .await;
        return Some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
    }

    let mut headers = files.headers(status.as_u16());
    headers.extend_from_slice(extra_headers);
    headers.extend(h3_static_validator_headers(files, &file));
    headers.push((
        HeaderName::from_static("content-type"),
        files.content_type(&file.name),
    ));
    headers.push((
        HeaderName::from_static("accept-ranges"),
        HeaderValue::from_static("bytes"),
    ));
    headers.push((
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("static length is a valid header"),
    ));
    if range.is_some() {
        headers.push((
            HeaderName::from_static("content-range"),
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", file.size))
                .expect("static range is a valid header"),
        ));
    }
    let status_code = status.as_u16();
    let mut response = Response::new(());
    *response.status_mut() = status;
    for (name, value) in headers {
        response.headers_mut().append(name, value);
    }
    let mut response_headers = response.headers().clone();
    if sanitize_h3_response_headers(&mut response_headers, status, length, head).is_err() {
        let _ = send_h3_error(
            stream,
            StatusCode::INTERNAL_SERVER_ERROR,
            b"static response headers are invalid\n",
            deadline,
        )
        .await;
        return Some(StatusCode::INTERNAL_SERVER_ERROR.as_u16());
    }
    *response.headers_mut() = response_headers;
    if !matches!(
        timeout_at(deadline, stream.send_response(response)).await,
        Ok(Ok(()))
    ) {
        return None;
    }
    if head || length == 0 {
        if !matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(()))) {
            return None;
        }
        return Some(status_code);
    }

    let mut file = TokioFile::from_std(file.file);
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return None;
    }
    let mut remaining = length;
    let mut buffer = vec![0; 64 * 1024];
    while remaining != 0 {
        let chunk_size = usize::try_from(remaining.min(buffer.len() as u64)).expect("chunk bound");
        let read = match timeout_at(deadline, file.read(&mut buffer[..chunk_size])).await {
            Ok(Ok(read)) if read != 0 => read,
            _ => return None,
        };
        remaining -= u64::try_from(read).expect("read length fits u64");
        if !matches!(
            timeout_at(
                deadline,
                stream.send_data(Bytes::copy_from_slice(&buffer[..read])),
            )
            .await,
            Ok(Ok(()))
        ) {
            return None;
        }
    }
    if !matches!(timeout_at(deadline, stream.finish()).await, Ok(Ok(()))) {
        return None;
    }
    Some(status_code)
}
