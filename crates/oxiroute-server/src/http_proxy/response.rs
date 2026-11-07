#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) async fn respond_error(
    session: &mut Session,
    code: u16,
    error: &Error,
    reason: Option<TimeoutReason>,
    pool: Option<&UpstreamPlan>,
) -> pingora::Result<()> {
    let pool_unavailable = is_pool_unavailable(error);
    if !pool_unavailable && reason.is_none() {
        return session.respond_error(code).await;
    }
    let mut response = ServerSession::generate_error(code);
    // Pingora's error responder carries a placeholder Date that only the automatic headers replace.
    response.insert_header(DATE, httpdate::fmt_http_date(SystemTime::now()))?;
    if let Some((name, value)) = reason.map(TimeoutReason::header) {
        response.insert_header(name, value)?;
    }
    let mut body = Bytes::new();
    if pool_unavailable {
        response.insert_header(POOL_UNAVAILABLE_HEADER, "1")?;
        if let Some(plan) = pool {
            body = unavailable_body(session, &mut response, plan.unavailable_response())?;
        }
    }
    session.write_error_response(response, body).await
}

fn unavailable_body(
    session: &Session,
    response: &mut pingora::http::ResponseHeader,
    unavailable: &UnavailableResponse,
) -> pingora::Result<Bytes> {
    if let Some(retry_after) = &unavailable.retry_after {
        response.insert_header(RETRY_AFTER, retry_after.clone())?;
    }
    if unavailable.body.is_empty() {
        return Ok(Bytes::new());
    }
    response.insert_header(CONTENT_TYPE, "text/plain; charset=utf-8")?;
    response.insert_header(CONTENT_LENGTH, unavailable.body.len().to_string())?;
    let head = session.req_header().method == Method::HEAD;
    Ok(if head {
        Bytes::new()
    } else {
        unavailable.body.clone()
    })
}

const REASON_HEADER: HeaderName = HeaderName::from_static("x-oxiroute-reason");

/// Which upstream or total timeout ended a request that has no complete response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimeoutReason {
    Connect,
    UpstreamHeader,
    UpstreamIdle,
    UpstreamWrite,
    TotalDeadline,
}

impl TimeoutReason {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect_timeout",
            Self::UpstreamHeader => "upstream_header_timeout",
            Self::UpstreamIdle => "upstream_idle_timeout",
            Self::UpstreamWrite => "upstream_write_timeout",
            Self::TotalDeadline => "total_deadline",
        }
    }

    pub(super) const fn header(self) -> (HeaderName, HeaderValue) {
        (REASON_HEADER, HeaderValue::from_static(self.as_str()))
    }
}

pub(super) fn timeout_reason(
    error: &Error,
    ctx: &HttpRequestContext,
    session: &Session,
) -> Option<TimeoutReason> {
    let timed_out = matches!(
        error.etype(),
        ErrorType::ConnectTimedout
            | ErrorType::TLSHandshakeTimedout
            | ErrorType::ReadTimedout
            | ErrorType::WriteTimedout
    );
    if timed_out && ctx.remaining(false).is_err() {
        return Some(TimeoutReason::TotalDeadline);
    }
    if error.esource() != &ErrorSource::Upstream {
        return None;
    }
    match error.etype() {
        ErrorType::ConnectTimedout | ErrorType::TLSHandshakeTimedout => {
            Some(TimeoutReason::Connect)
        }
        ErrorType::WriteTimedout => Some(TimeoutReason::UpstreamWrite),
        ErrorType::ReadTimedout if response_started(session) => Some(TimeoutReason::UpstreamIdle),
        ErrorType::ReadTimedout => Some(TimeoutReason::UpstreamHeader),
        _ => None,
    }
}

fn response_started(session: &Session) -> bool {
    session
        .response_written()
        .is_some_and(|response| !response.status.is_informational())
}

pub(super) fn proxy_error_status(error: &Error) -> u16 {
    match error.etype() {
        ErrorType::HTTPStatus(code) => *code,
        _ if is_pool_unavailable(error) => 503,
        _ => match error.esource() {
            ErrorSource::Upstream => 502,
            ErrorSource::Downstream => match error.etype() {
                ErrorType::WriteError | ErrorType::ReadError | ErrorType::ConnectionClosed => 0,
                _ => 400,
            },
            ErrorSource::Internal | ErrorSource::Unset => 500,
        },
    }
}

pub(super) fn nginx_server_marker(headers: &[(HeaderName, HeaderValue)]) -> Option<HeaderValue> {
    let html = headers.iter().any(|(name, value)| {
        name == CONTENT_TYPE && value.as_bytes().eq_ignore_ascii_case(b"text/html")
    });
    html.then(|| {
        headers
            .iter()
            .rev()
            .find(|(name, _)| name == SERVER)
            .map(|(_, value)| value.clone())
    })
    .flatten()
}

pub(super) fn nginx_error_body(status: u16, reason: &str, server: &HeaderValue) -> Bytes {
    let server = server
        .to_str()
        .expect("validated nginx server marker is visible ASCII");
    Bytes::from(format!(
        "<html>\r\n<head><title>{status} {reason}</title></head>\r\n<body>\r\n<center><h1>{status} {reason}</h1></center>\r\n<hr><center>{server}</center>\r\n</body>\r\n</html>\r\n"
    ))
}

pub(crate) fn apply_response_policy(
    response: &mut pingora::http::ResponseHeader,
    policy: &ProxyPolicyPlan,
    remove_hop_by_hop: bool,
) -> pingora::Result<()> {
    let decisions = decide_response_headers(
        response.status,
        &response.headers,
        policy,
        remove_hop_by_hop,
    )
    .map_err(|error| match error {
        ResponsePolicyError::InvalidConnectionNomination => {
            Error::new_up(ErrorType::InvalidHTTPHeader)
        }
        ResponsePolicyError::InvalidCookie => Error::new_in(ErrorType::InvalidHTTPHeader),
    })?;
    for decision in decisions {
        match decision {
            ResponseHeaderDecision::Remove(name) => {
                response.remove_header(&name);
            }
            ResponseHeaderDecision::Set { name, value } => {
                response.insert_header(name, value)?;
            }
            ResponseHeaderDecision::Add { name, value } => {
                response.append_header(name, value)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_response_policy_map(
    status: StatusCode,
    headers: &mut HeaderMap,
    policy: &ProxyPolicyPlan,
) -> pingora::Result<()> {
    let decisions =
        decide_response_headers(status, headers, policy, true).map_err(|error| match error {
            ResponsePolicyError::InvalidConnectionNomination => {
                Error::new_up(ErrorType::InvalidHTTPHeader)
            }
            ResponsePolicyError::InvalidCookie => Error::new_in(ErrorType::InvalidHTTPHeader),
        })?;
    for decision in decisions {
        match decision {
            ResponseHeaderDecision::Remove(name) => {
                headers.remove(name);
            }
            ResponseHeaderDecision::Set { name, value } => {
                headers.insert(name, value);
            }
            ResponseHeaderDecision::Add { name, value } => {
                headers.append(name, value);
            }
        }
    }
    Ok(())
}

pub(super) fn observe_counter(
    listener: &ListenerMetrics,
    current: usize,
    observed: &mut u64,
    received: bool,
) {
    let Ok(current) = u64::try_from(current) else {
        warn!("HTTP byte counter exceeds the supported range");
        return;
    };
    let Some(delta) = current.checked_sub(*observed) else {
        warn!("HTTP byte counter moved backwards");
        return;
    };
    if delta == 0 {
        return;
    }
    let result = if received {
        listener.record_bytes_received(delta)
    } else {
        listener.record_bytes_sent(delta)
    };
    match result {
        Ok(()) => *observed = current,
        Err(error) => warn!("could not account for HTTP traffic: {error}"),
    }
}

pub(super) fn classify_http_result(
    error: Option<&Error>,
    response_status: Option<u16>,
) -> HttpOperationResult {
    let Some(error) = error else {
        return HttpOperationResult::from_status(response_status);
    };
    if matches!(
        error.etype(),
        ErrorType::ConnectTimedout
            | ErrorType::TLSHandshakeTimedout
            | ErrorType::ReadTimedout
            | ErrorType::WriteTimedout
    ) {
        return HttpOperationResult::Timeout;
    }
    if matches!(error.etype(), ErrorType::ConnectionClosed) {
        return HttpOperationResult::Cancelled;
    }
    if matches!(error.esource(), pingora::ErrorSource::Upstream) {
        return HttpOperationResult::UpstreamError;
    }
    if let ErrorType::HTTPStatus(status) = error.etype() {
        return HttpOperationResult::from_status(Some(*status));
    }
    HttpOperationResult::InternalError
}
