#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn internal_uri(path: &str, previous: &http::Uri) -> pingora::Result<http::Uri> {
    let path_and_query = previous
        .query()
        .map_or_else(|| path.to_owned(), |query| format!("{path}?{query}"));
    path_and_query
        .parse()
        .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

pub(crate) fn rewrite_upstream_path(
    uri: &mut http::Uri,
    rewrite: &HttpProxyPathRewrite,
) -> pingora::Result<()> {
    let Some(suffix) = uri.path().strip_prefix(&rewrite.from) else {
        return Err(Error::new_in(ErrorType::InvalidHTTPHeader));
    };
    let path = format!("{}{}", rewrite.to, suffix);
    let path_and_query = uri
        .query()
        .map_or_else(|| path.clone(), |query| format!("{path}?{query}"));
    *uri = path_and_query
        .parse()
        .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))?;
    Ok(())
}

pub(super) fn internal_location(path: &str, previous: &http::Uri) -> pingora::Result<HeaderValue> {
    let uri = internal_uri(path, previous)?;
    HeaderValue::from_str(
        uri.path_and_query()
            .map_or(uri.path(), http::uri::PathAndQuery::as_str),
    )
    .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
}

pub(super) fn proxy_policy(ctx: &HttpRequestContext) -> &ProxyPolicyPlan {
    &proxy_action(ctx).policy
}

pub(super) fn proxy_action(ctx: &HttpRequestContext) -> &ProxyActionPlan {
    let HttpActionPlan::Proxy(proxy) = &proxy_route(ctx).action else {
        unreachable!("upstream hooks only run for proxy actions");
    };
    proxy
}

/// The server a sticky session pins the first attempt to; later attempts choose freely.
pub(super) fn affinity_server(session: &Session, ctx: &HttpRequestContext) -> Option<String> {
    if ctx.has_attempted() {
        return None;
    }
    let request = session.req_header();
    let client_ip = session
        .client_addr()
        .and_then(|address| address.as_inet())
        .map(std::net::SocketAddr::ip);
    proxy_policy(ctx).affinity.as_ref()?.preferred(
        ctx.pool.as_ref()?.selector(),
        &AffinityRequest {
            headers: &request.headers,
            client_ip,
            path: request.uri.path(),
        },
    )
}

/// Pins the client to the server that answered, unless its cookie already names it.
pub(super) fn apply_affinity_cookie(
    session: &Session,
    ctx: &HttpRequestContext,
    response: &mut pingora::http::ResponseHeader,
) -> pingora::Result<()> {
    let (Some(affinity), Some(pool), Some(server)) = (
        proxy_policy(ctx).affinity.as_ref(),
        ctx.pool.as_ref(),
        ctx.trace.servers.last(),
    ) else {
        return Ok(());
    };
    match affinity.set_cookie(pool.selector(), server, &session.req_header().headers) {
        Some(cookie) => response
            .append_header(http::header::SET_COOKIE, cookie)
            .map(drop),
        None => Ok(()),
    }
}

pub(super) fn proxy_route(ctx: &HttpRequestContext) -> &HttpRoutePlan {
    ctx.route.as_ref().expect("proxy route context")
}

pub(super) fn pingora_request_header_decisions(
    session: &Session,
    route: &HttpRoutePlan,
    authority: Option<&Authority>,
) -> Result<Box<[RequestHeaderDecision]>, RequestPolicyError> {
    let HttpActionPlan::Proxy(proxy) = &route.action else {
        return Ok(Box::new([]));
    };
    let client_ip = session
        .client_addr()
        .and_then(|address| address.as_inet())
        .map(std::net::SocketAddr::ip);
    let downstream_scheme = if session
        .digest()
        .and_then(|digest| digest.ssl_digest.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    let context = RequestPolicyContext {
        authority,
        downstream_scheme,
        client_ip,
        incoming_headers: &session.req_header().headers,
    };
    let mut decisions = Vec::with_capacity(proxy.policy.request_headers.len());
    for mutation in &proxy.policy.request_headers {
        if mutation.is_pingora_managed_upgrade() {
            continue;
        }
        decisions.push(decide_request_header(mutation, context)?);
    }
    if let Some(select) = proxy.select.as_ref().filter(|select| !select.forward) {
        decisions.push(RequestHeaderDecision::Remove(select.header().clone()));
    }
    Ok(decisions.into_boxed_slice())
}

pub(super) fn pingora_request_policy_error(error: RequestPolicyError) -> Error {
    *match error {
        RequestPolicyError::SourceTooLarge => Error::new_down(ErrorType::HTTPStatus(431)),
        RequestPolicyError::InvalidHeader => Error::new_in(ErrorType::InvalidHTTPHeader),
        RequestPolicyError::ClientIpUnavailable => {
            Error::new_in(ErrorType::Custom("ClientIpUnavailable"))
        }
        RequestPolicyError::SelectedUpstreamHostUnavailable => {
            Error::new_in(ErrorType::InternalError)
        }
    }
}

/// The request's `Content-Length`, without a lookup for the usual request that has none.
pub(super) fn request_content_length(
    session: &Session,
    facts: RequestFacts,
) -> Result<Option<u64>, ()> {
    if facts.content_lengths == 0 {
        return Ok(None);
    }
    content_length(&session.req_header().headers)
}

pub(super) fn content_length(headers: &http::HeaderMap) -> Result<Option<u64>, ()> {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let value = match values.next() {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or(())?,
        ),
        None => None,
    };
    if values.next().is_some() {
        return Err(());
    }
    Ok(value)
}

pub(crate) fn selected_upstream_host(
    endpoint: &RuntimeEndpoint,
    policy: &HttpUpstreamHost,
    incoming: Option<&Authority>,
) -> pingora::Result<Option<HeaderValue>> {
    let host_value = |text: &str| {
        HeaderValue::from_str(text)
            .map(Some)
            .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))
    };
    match policy {
        HttpUpstreamHost::PreserveIncoming => {
            incoming.map_or(Ok(None), |incoming| host_value(incoming.as_str()))
        }
        HttpUpstreamHost::NginxHost { fallback } => {
            let fallback = HeaderValue::from_str(fallback)
                .map_err(|_| Error::new_in(ErrorType::InvalidHTTPHeader))?;
            Ok(nginx_request_host(incoming, &fallback)
                .map(Some)
                .map_err(pingora_request_policy_error)?)
        }
        HttpUpstreamHost::Endpoint { unix_fallback } => match endpoint {
            RuntimeEndpoint::Socket { address } => host_value(&address.to_string()),
            RuntimeEndpoint::Dns { host, port } => host_value(&format!("{host}:{port}")),
            RuntimeEndpoint::Unix { .. } => {
                host_value(unix_fallback.as_deref().unwrap_or_default())
            }
        },
        HttpUpstreamHost::Literal { value } => host_value(value),
    }
}

/// [`selected_upstream_host`] for a request that is at hand: an origin-form request whose host is passed on
/// as it came has its `Host` value, already built, instead of a copy made from the parsed authority.
pub(super) fn selected_upstream_host_for(
    request: &pingora::http::RequestHeader,
    endpoint: &RuntimeEndpoint,
    policy: &HttpUpstreamHost,
    incoming: Option<&Authority>,
) -> pingora::Result<Option<HeaderValue>> {
    if matches!(policy, HttpUpstreamHost::PreserveIncoming)
        && incoming.is_some()
        && request.uri.authority().is_none()
    {
        return Ok(request.headers.get(HOST).cloned());
    }
    selected_upstream_host(endpoint, policy, incoming)
}

pub(super) fn apply_request_header_mutations(
    request: &mut pingora::http::RequestHeader,
    ctx: &HttpRequestContext,
) -> pingora::Result<()> {
    for decision in &ctx.request_header_decisions {
        match decision {
            RequestHeaderDecision::Remove(name) => {
                request.remove_header(name);
            }
            RequestHeaderDecision::Set { name, value } => {
                let Some(value) = value
                    .complete(ctx.selected_upstream_host.as_ref())
                    .map_err(pingora_request_policy_error)?
                else {
                    continue;
                };
                request.insert_header(name.clone(), value)?;
            }
        }
    }
    Ok(())
}

fn connection_tokens(headers: &http::HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// A client's `close` option describes its own connection. Forwarded, it makes the origin close, and
/// the pool discard, a connection that could serve the next request.
pub(super) fn has_downstream_close(headers: &http::HeaderMap) -> bool {
    headers.get_all(CONNECTION).iter().any(|value| {
        value
            .as_bytes()
            .split(|byte| *byte == b',')
            .any(|token| token.trim_ascii().eq_ignore_ascii_case(b"close"))
    })
}

pub(super) fn remove_downstream_close(
    request: &mut pingora::http::RequestHeader,
) -> pingora::Result<()> {
    if !has_downstream_close(&request.headers) {
        return Ok(());
    }
    let kept = connection_tokens(&request.headers)
        .filter(|token| !token.eq_ignore_ascii_case("close"))
        .collect::<Vec<_>>()
        .join(", ");
    request.remove_header(&CONNECTION);
    if !kept.is_empty() {
        request.insert_header(CONNECTION, kept)?;
    }
    Ok(())
}

pub(super) fn upstream_request_requires_mutation(
    session: &Session,
    ctx: &HttpRequestContext,
) -> bool {
    let request = session.req_header();
    if ctx.cache_transaction.is_some() || proxy_policy(ctx).upstream_path_rewrite.is_some() {
        return true;
    }
    if request.version == http::Version::HTTP_10
        || (ctx.request_facts.connection && has_downstream_close(&request.headers))
    {
        return true;
    }
    let host_requires_mutation = match &ctx.selected_upstream_host {
        Some(selected) => !has_single_canonical_host(request, selected),
        None => request.headers.contains_key(HOST),
    };
    if host_requires_mutation
        || ctx.pool.as_ref().is_some_and(|pool| {
            pool.connection_reuse() == oxiroute_config::UpstreamConnectionReuse::Never
        })
    {
        return true;
    }

    ctx.request_header_decisions
        .iter()
        .any(RequestHeaderDecision::requires_mutation)
}

pub(super) fn has_single_canonical_host(
    request: &pingora::http::RequestHeader,
    selected: &HeaderValue,
) -> bool {
    let mut hosts = request.headers.get_all(HOST).iter();
    let Some(value) = hosts.next() else {
        return false;
    };
    if value != selected || hosts.next().is_some() {
        return false;
    }
    if !request.has_case() {
        return true;
    }
    request
        .case_header_iter()
        .find(|(name, _)| name.as_slice().eq_ignore_ascii_case(b"host"))
        .is_some_and(|(name, _)| name.as_slice() == b"Host")
}

pub(super) fn request_authority(
    request: &pingora::http::RequestHeader,
) -> Result<Option<Authority>, ()> {
    let uri_authority = request.uri.authority().cloned();
    let mut host_values = request.headers.get_all(HOST).iter();
    let host_authority = host_values
        .next()
        .map(|host| {
            host.to_str()
                .map_err(|_| ())?
                .parse::<Authority>()
                .map_err(|_| ())
        })
        .transpose()?;
    if host_values.next().is_some() {
        return Err(());
    }
    if uri_authority
        .iter()
        .chain(host_authority.iter())
        .any(|authority| authority.as_str().contains('@'))
    {
        return Err(());
    }

    match (uri_authority, host_authority) {
        (Some(uri), Some(host)) if uri != host => Err(()),
        (Some(uri), _) => Ok(Some(uri)),
        (None, Some(host)) => Ok(Some(host)),
        (None, None) if request.version == http::Version::HTTP_11 => Err(()),
        (None, None) => Ok(None),
    }
}
