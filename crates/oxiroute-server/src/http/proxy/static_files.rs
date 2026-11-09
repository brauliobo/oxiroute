#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) async fn write_static_error(
    session: &mut Session,
    files: &crate::http::action::StaticFilesPlan,
    status: u16,
    head: bool,
    extra_headers: &[(HeaderName, HeaderValue)],
) -> pingora::Result<Option<(String, Vec<(HeaderName, HeaderValue)>)>> {
    if let Some(target) = files.error_document(status).await {
        match target {
            StaticErrorTarget::File { file, headers } => {
                let mut response_headers = headers.into_vec();
                response_headers.extend_from_slice(extra_headers);
                write_static_file_with_status(
                    session,
                    files,
                    file,
                    status,
                    head,
                    None,
                    &response_headers,
                )
                .await?;
                Ok(None)
            }
            StaticErrorTarget::InternalRedirect { path, headers } => {
                Ok(Some((path, headers.into_vec())))
            }
            StaticErrorTarget::Literal { body, headers } => {
                let mut response_headers = files.headers(status);
                response_headers.extend(headers);
                response_headers.extend_from_slice(extra_headers);
                write_local_response(session, status, &response_headers, body, head).await?;
                Ok(None)
            }
        }
    } else {
        let mut headers = files.headers(status);
        headers.extend_from_slice(extra_headers);
        write_local_response(session, status, &headers, Bytes::new(), head).await?;
        Ok(None)
    }
}

pub(super) async fn write_static_file(
    session: &mut Session,
    files: &crate::http::action::StaticFilesPlan,
    file: StaticFile,
    head: bool,
) -> pingora::Result<Option<Vec<(HeaderName, HeaderValue)>>> {
    match files.request_decision(&session.req_header().headers, &file) {
        StaticRequestDecision::NotModified => {
            let mut headers = files.headers(304);
            headers.extend(static_validator_headers(files, &file));
            write_local_response(session, 304, &headers, Bytes::new(), true).await?;
            return Ok(None);
        }
        StaticRequestDecision::PreconditionFailed => {
            let mut headers = files.headers(412);
            headers.extend(static_validator_headers(files, &file));
            write_local_response(session, 412, &headers, Bytes::new(), head).await?;
            return Ok(None);
        }
        StaticRequestDecision::RangeNotSatisfiable => {
            return Ok(Some(vec![
                (ACCEPT_RANGES, HeaderValue::from_static("bytes")),
                (
                    CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", file.size))
                        .expect("static size is a valid header value"),
                ),
            ]));
        }
        StaticRequestDecision::Serve { range } => {
            let status = if range.is_some() { 206 } else { 200 };
            write_static_file_with_status(session, files, file, status, head, range, &[]).await?;
        }
    }
    Ok(None)
}

pub(super) async fn write_static_file_with_status(
    session: &mut Session,
    files: &crate::http::action::StaticFilesPlan,
    file: StaticFile,
    status: u16,
    head: bool,
    range: Option<(u64, u64)>,
    extra_headers: &[(HeaderName, HeaderValue)],
) -> pingora::Result<()> {
    let (start, end) = range.unwrap_or_else(|| (0, file.size.saturating_sub(1)));
    let length = if file.size == 0 { 0 } else { end - start + 1 };
    let configured = files.headers(status);
    let mut response = pingora::http::ResponseHeader::build(
        status,
        Some(configured.len() + extra_headers.len() + 7),
    )?;
    for (name, value) in configured.into_iter().chain(extra_headers.iter().cloned()) {
        response.append_header(name, value)?;
    }
    if files.etag_enabled() {
        response.append_header(ETAG, file.etag.clone())?;
    }
    response.append_header(LAST_MODIFIED, file.last_modified.clone())?;
    response.insert_header(CONTENT_TYPE, file.content_type.clone())?;
    response.insert_header(ACCEPT_RANGES, "bytes")?;
    response.insert_header(CONTENT_LENGTH, HeaderValue::from(length))?;
    if range.is_some() {
        response.insert_header(CONTENT_RANGE, format!("bytes {start}-{end}/{}", file.size))?;
    }
    if !session.is_body_empty() {
        session.set_close_on_response_before_downstream_finish(true);
    }
    write_response_header_with_id(session, Box::new(response), head || length == 0).await?;
    if head || length == 0 {
        return Ok(());
    }

    let mut reader = file.content.into_reader(start, length);
    let read_error = |error| Error::because(ErrorType::ReadError, "static file read failed", error);
    let mut chunk = reader.next_chunk().await.map_err(read_error)?;
    while let Some(current) = chunk {
        chunk = reader.next_chunk().await.map_err(read_error)?;
        session
            .write_response_body(Some(current), chunk.is_none())
            .await?;
    }
    Ok(())
}

fn static_validator_headers(
    files: &crate::http::action::StaticFilesPlan,
    file: &StaticFile,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut headers = Vec::with_capacity(usize::from(files.etag_enabled()) + 1);
    if files.etag_enabled() {
        headers.push((ETAG, file.etag.clone()));
    }
    headers.push((LAST_MODIFIED, file.last_modified.clone()));
    headers
}
