use std::{borrow::Cow, net::IpAddr};

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::SET_COOKIE, uri::Authority};
use oxiroute_config::{CookiePathMatch, HttpCookiePathRewrite, HttpSameSite};

use crate::http::action::{
    MAX_REDIRECT_LOCATION_BYTES, ProxyPolicyPlan, RedirectLocationPlan, RedirectTemplateSegment,
    RequestHeaderMutationPlan, RequestHeaderValuePlan, ResponseHeaderMutationPlan,
};
use crate::http::hop_by_hop::{has_hop_by_hop, hop_by_hop_removals};

/// The headers of the request a policy reads: a parsed map, or the scanned head of the fast lane.
pub(crate) trait IncomingHeaders {
    /// Appends the values of every header called `name` to `out`, joined by `, ` in arrival order, and says whether
    /// anything was appended.
    fn join_into(
        &self,
        name: &HeaderName,
        max_bytes: usize,
        out: &mut Vec<u8>,
    ) -> Result<bool, RequestPolicyError>;

    /// The values of every header called `name`, joined by `, ` in arrival order; `None` when there are none.
    #[cfg(test)]
    fn joined(
        &self,
        name: &HeaderName,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RequestPolicyError> {
        let mut joined = Vec::new();
        Ok(self
            .join_into(name, max_bytes, &mut joined)?
            .then_some(joined))
    }
}

impl IncomingHeaders for HeaderMap {
    fn join_into(
        &self,
        name: &HeaderName,
        max_bytes: usize,
        out: &mut Vec<u8>,
    ) -> Result<bool, RequestPolicyError> {
        join_values_into(
            out,
            self.get_all(name).iter().map(HeaderValue::as_bytes),
            max_bytes,
        )
    }
}

pub(crate) struct RequestPolicyContext<'a, H: IncomingHeaders + ?Sized = HeaderMap> {
    pub(crate) authority: Option<&'a Authority>,
    pub(crate) downstream_scheme: &'static str,
    pub(crate) client_ip: Option<IpAddr>,
    pub(crate) incoming_headers: &'a H,
}

impl<H: IncomingHeaders + ?Sized> Clone for RequestPolicyContext<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: IncomingHeaders + ?Sized> Copy for RequestPolicyContext<'_, H> {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RequestHeaderDecision {
    Remove(HeaderName),
    Set {
        name: HeaderName,
        value: RequestHeaderValueDecision,
    },
}

impl RequestHeaderDecision {
    pub(crate) fn requires_mutation(&self) -> bool {
        match self {
            Self::Remove(_) => true,
            Self::Set { value, .. } => value.requires_mutation(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RequestHeaderValueDecision {
    Value(HeaderValue),
    ClientIp(Option<HeaderValue>),
    XForwardedFor(XForwardedForDecision),
    SelectedUpstreamHost,
}

impl RequestHeaderValueDecision {
    fn requires_mutation(&self) -> bool {
        !matches!(
            self,
            Self::XForwardedFor(XForwardedForDecision::Preserve | XForwardedForDecision::NoOp)
        )
    }

    pub(crate) fn complete(
        &self,
        selected_upstream_host: Option<&HeaderValue>,
    ) -> Result<Option<HeaderValue>, RequestPolicyError> {
        self.resolve(selected_upstream_host)
            .map(<Option<&HeaderValue>>::cloned)
    }

    /// The value the header is set to, borrowed; `None` when the header is left as it is.
    pub(crate) fn resolve<'a>(
        &'a self,
        selected_upstream_host: Option<&'a HeaderValue>,
    ) -> Result<Option<&'a HeaderValue>, RequestPolicyError> {
        match self {
            Self::Value(value) | Self::XForwardedFor(XForwardedForDecision::Set(value)) => {
                Ok(Some(value))
            }
            Self::ClientIp(value) => value
                .as_ref()
                .map(Some)
                .ok_or(RequestPolicyError::ClientIpUnavailable),
            Self::XForwardedFor(XForwardedForDecision::Preserve | XForwardedForDecision::NoOp) => {
                Ok(None)
            }
            Self::SelectedUpstreamHost => selected_upstream_host
                .map(Some)
                .ok_or(RequestPolicyError::SelectedUpstreamHostUnavailable),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum XForwardedForDecision {
    Preserve,
    NoOp,
    Set(HeaderValue),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestPolicyError {
    SourceTooLarge,
    InvalidHeader,
    ClientIpUnavailable,
    SelectedUpstreamHostUnavailable,
}

/// What the value of a request rule came out as for a request: the bytes of it are in the caller's buffer for
/// `Value`, `ClientIp` and `ForwardedFor`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Rendered {
    Value,
    ClientIp,
    /// The rule needs the client address and the listener has none.
    ClientIpMissing,
    ForwardedFor,
    /// The client is an exception: the header goes on as it came.
    Preserve,
    /// No client address and nothing arrived: nothing to set.
    NoOp,
    SelectedUpstreamHost,
}

/// Appends the value a request rule sets for the request in `context` to `out`. The one place the values are
/// computed: the classic decision wraps the bytes in a `HeaderValue`, the fast lane writes them to the head as they
/// are. The bytes are not yet checked for being a valid header value.
pub(crate) fn render_request_value<H: IncomingHeaders + ?Sized>(
    plan: &RequestHeaderValuePlan,
    context: RequestPolicyContext<'_, H>,
    out: &mut Vec<u8>,
) -> Result<Rendered, RequestPolicyError> {
    match plan {
        RequestHeaderValuePlan::Literal(value) => out.extend_from_slice(value.as_bytes()),
        RequestHeaderValuePlan::IncomingAuthority => out.extend_from_slice(
            context
                .authority
                .map(Authority::as_str)
                .unwrap_or_default()
                .as_bytes(),
        ),
        RequestHeaderValuePlan::NormalizedHost => {
            let host = context.authority.map(normalized_request_host);
            out.extend_from_slice(host.as_deref().unwrap_or_default().as_bytes());
        }
        RequestHeaderValuePlan::NginxHost { fallback } => {
            render_nginx_host(context.authority, fallback, out);
        }
        RequestHeaderValuePlan::ClientIp => {
            let Some(client_ip) = context.client_ip else {
                return Ok(Rendered::ClientIpMissing);
            };
            out.extend_from_slice(IpText::new(client_ip).as_bytes());
            return Ok(Rendered::ClientIp);
        }
        RequestHeaderValuePlan::AppendedXForwardedFor {
            max_bytes,
            except_source_cidrs,
        } => return render_x_forwarded_for(context, *max_bytes, except_source_cidrs, out),
        RequestHeaderValuePlan::DownstreamScheme => {
            out.extend_from_slice(context.downstream_scheme.as_bytes());
        }
        RequestHeaderValuePlan::IncomingHeader { name, max_bytes } => {
            context.incoming_headers.join_into(name, *max_bytes, out)?;
        }
        RequestHeaderValuePlan::SelectedUpstreamHost => return Ok(Rendered::SelectedUpstreamHost),
    }
    Ok(Rendered::Value)
}

pub(crate) fn decide_request_header<H: IncomingHeaders + ?Sized>(
    mutation: &RequestHeaderMutationPlan,
    context: RequestPolicyContext<'_, H>,
) -> Result<RequestHeaderDecision, RequestPolicyError> {
    let (name, plan) = match mutation {
        RequestHeaderMutationPlan::Set { name, value } => (name, value),
        RequestHeaderMutationPlan::Remove { name } => {
            return Ok(RequestHeaderDecision::Remove(name.clone()));
        }
    };
    let value = match plan {
        RequestHeaderValuePlan::Literal(value) => RequestHeaderValueDecision::Value(value.clone()),
        plan => {
            let mut bytes = Vec::new();
            let rendered = render_request_value(plan, context, &mut bytes)?;
            let value =
                || HeaderValue::from_bytes(&bytes).map_err(|_| RequestPolicyError::InvalidHeader);
            match rendered {
                // nginx does not send a header whose value is empty, which `$host` is for an empty primary name.
                Rendered::Value
                    if bytes.is_empty()
                        && matches!(plan, RequestHeaderValuePlan::NginxHost { .. }) =>
                {
                    return Ok(RequestHeaderDecision::Remove(name.clone()));
                }
                Rendered::Value => RequestHeaderValueDecision::Value(value()?),
                Rendered::ClientIp => RequestHeaderValueDecision::ClientIp(Some(value()?)),
                Rendered::ClientIpMissing => RequestHeaderValueDecision::ClientIp(None),
                Rendered::ForwardedFor => {
                    RequestHeaderValueDecision::XForwardedFor(XForwardedForDecision::Set(value()?))
                }
                Rendered::Preserve => {
                    RequestHeaderValueDecision::XForwardedFor(XForwardedForDecision::Preserve)
                }
                Rendered::NoOp => {
                    RequestHeaderValueDecision::XForwardedFor(XForwardedForDecision::NoOp)
                }
                Rendered::SelectedUpstreamHost => RequestHeaderValueDecision::SelectedUpstreamHost,
            }
        }
    };
    Ok(RequestHeaderDecision::Set {
        name: name.clone(),
        value,
    })
}

fn render_x_forwarded_for<H: IncomingHeaders + ?Sized>(
    context: RequestPolicyContext<'_, H>,
    max_bytes: usize,
    exceptions: &[crate::http::action::SourceCidr],
    out: &mut Vec<u8>,
) -> Result<Rendered, RequestPolicyError> {
    if context.client_ip.is_some_and(|client_ip| {
        exceptions
            .iter()
            .any(|exception| exception.contains(client_ip))
    }) {
        return Ok(Rendered::Preserve);
    }
    let name = HeaderName::from_static("x-forwarded-for");
    let start = out.len();
    let existing = context.incoming_headers.join_into(&name, max_bytes, out)?;
    let Some(client_ip) = context.client_ip else {
        return Ok(if existing {
            Rendered::ForwardedFor
        } else {
            Rendered::NoOp
        });
    };
    let client_ip = IpText::new(client_ip);
    if existing {
        extend_bounded(out, start, b", ", max_bytes)?;
    }
    extend_bounded(out, start, client_ip.as_bytes(), max_bytes)?;
    Ok(Rendered::ForwardedFor)
}

/// The text of an address in a stack buffer: no allocation, and IPv4 without the formatter.
pub(crate) struct IpText {
    buf: [u8; 45],
    len: usize,
}

impl IpText {
    pub(crate) fn new(address: IpAddr) -> Self {
        let mut text = Self {
            buf: [0; 45],
            len: 0,
        };
        match address {
            IpAddr::V4(address) => {
                for (index, octet) in address.octets().into_iter().enumerate() {
                    if index > 0 {
                        text.push(b'.');
                    }
                    if octet >= 100 {
                        text.push(b'0' + octet / 100);
                    }
                    if octet >= 10 {
                        text.push(b'0' + octet / 10 % 10);
                    }
                    text.push(b'0' + octet % 10);
                }
            }
            IpAddr::V6(address) => {
                let mut rest = &mut text.buf[..];
                std::io::Write::write_fmt(&mut rest, format_args!("{address}"))
                    .expect("an IPv6 address fits 45 bytes");
                text.len = 45 - rest.len();
            }
        }
        text
    }

    /// An IPv4 socket address as `address:port`; `None` for IPv6, whose text carries a scope.
    pub(crate) fn socket(address: std::net::SocketAddr) -> Option<Self> {
        let std::net::SocketAddr::V4(address) = address else {
            return None;
        };
        let mut text = Self::new(IpAddr::V4(*address.ip()));
        text.push(b':');
        let port = address.port();
        for divisor in [10_000, 1_000, 100, 10] {
            if port >= divisor {
                text.push(b'0' + (port / divisor % 10) as u8);
            }
        }
        text.push(b'0' + (port % 10) as u8);
        Some(text)
    }

    fn push(&mut self, byte: u8) {
        self.buf[self.len] = byte;
        self.len += 1;
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// Appends the `values` to `out` joined by `, `, bounded as a whole to `max_bytes`; whether anything was appended.
pub(crate) fn join_values_into<'v>(
    out: &mut Vec<u8>,
    values: impl Iterator<Item = &'v [u8]>,
    max_bytes: usize,
) -> Result<bool, RequestPolicyError> {
    let start = out.len();
    for value in values {
        if out.len() > start {
            extend_bounded(out, start, b", ", max_bytes)?;
        }
        extend_bounded(out, start, value, max_bytes)?;
    }
    Ok(out.len() > start)
}

/// Appends `value` to the text that starts at `start` in `out`, unless the text would grow past `max_bytes`.
fn extend_bounded(
    out: &mut Vec<u8>,
    start: usize,
    value: &[u8],
    max_bytes: usize,
) -> Result<(), RequestPolicyError> {
    if (out.len() - start)
        .checked_add(value.len())
        .is_none_or(|length| length > max_bytes)
    {
        return Err(RequestPolicyError::SourceTooLarge);
    }
    out.extend_from_slice(value);
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResponseHeaderDecision {
    Remove(HeaderName),
    Set {
        name: HeaderName,
        value: HeaderValue,
    },
    Add {
        name: HeaderName,
        value: HeaderValue,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponsePolicyError {
    InvalidConnectionNomination,
    InvalidCookie,
}

pub(crate) fn decide_response_headers(
    status: StatusCode,
    headers: &HeaderMap,
    policy: &ProxyPolicyPlan,
    remove_hop_by_hop: bool,
) -> Result<Vec<ResponseHeaderDecision>, ResponsePolicyError> {
    let rewrites_cookies =
        !(policy.cookie_path_rewrites.is_empty() && policy.cookie_attributes.is_empty());
    // Only cookie rewriting reads the headers as they are after the decisions, so only it needs a copy.
    let mut resulting = rewrites_cookies.then(|| headers.clone());
    let mut decisions = Vec::new();
    // `Connection` is one of them, so without any there are no nominations either.
    if remove_hop_by_hop && has_hop_by_hop(headers) {
        for name in hop_by_hop_removals(headers)
            .map_err(|_| ResponsePolicyError::InvalidConnectionNomination)?
        {
            if let Some(resulting) = &mut resulting {
                resulting.remove(&name);
            }
            decisions.push(ResponseHeaderDecision::Remove(name));
        }
    }
    for mutation in policy
        .response_headers
        .iter()
        .filter(|mutation| mutation.applies(status.as_u16()))
    {
        match mutation {
            ResponseHeaderMutationPlan::Set { name, value, .. } => {
                if let Some(resulting) = &mut resulting {
                    resulting.insert(name.clone(), value.clone());
                }
                decisions.push(ResponseHeaderDecision::Set {
                    name: name.clone(),
                    value: value.clone(),
                });
            }
            ResponseHeaderMutationPlan::Add { name, value, .. } => {
                if let Some(resulting) = &mut resulting {
                    resulting.append(name.clone(), value.clone());
                }
                decisions.push(ResponseHeaderDecision::Add {
                    name: name.clone(),
                    value: value.clone(),
                });
            }
            ResponseHeaderMutationPlan::Remove { name } => {
                if let Some(resulting) = &mut resulting {
                    resulting.remove(name);
                }
                decisions.push(ResponseHeaderDecision::Remove(name.clone()));
            }
        }
    }
    let Some(resulting) = resulting else {
        return Ok(decisions);
    };
    let cookies = resulting
        .get_all(SET_COOKIE)
        .iter()
        .map(|cookie| rewrite_cookie(cookie, policy))
        .collect::<Result<Vec<_>, _>>()?;
    decisions.push(ResponseHeaderDecision::Remove(SET_COOKIE));
    decisions.extend(
        cookies
            .into_iter()
            .map(|value| ResponseHeaderDecision::Add {
                name: SET_COOKIE,
                value,
            }),
    );
    Ok(decisions)
}

fn rewrite_cookie(
    cookie: &HeaderValue,
    policy: &ProxyPolicyPlan,
) -> Result<HeaderValue, ResponsePolicyError> {
    let Ok(cookie_text) = cookie.to_str() else {
        return Ok(cookie.clone());
    };
    let mut segments = cookie_text.split(';');
    let first = segments.next().unwrap_or_default();
    let Some((cookie_name, _)) = first.trim_start_matches([' ', '\t']).split_once('=') else {
        return Ok(cookie.clone());
    };
    if cookie_name.is_empty() {
        return Ok(cookie.clone());
    }
    let attributes = policy
        .cookie_attributes
        .iter()
        .find(|candidate| candidate.name == cookie_name);
    let mut rewritten = first.to_owned();
    let mut saw_secure = false;
    let mut saw_http_only = false;
    let mut saw_same_site = false;
    for segment in segments {
        let trimmed = segment.trim_start_matches([' ', '\t']);
        let whitespace = &segment[..segment.len() - trimmed.len()];
        let (name, value) = trimmed
            .split_once('=')
            .map_or((trimmed, None), |(name, value)| (name, Some(value)));
        if name.eq_ignore_ascii_case("secure") {
            saw_secure = true;
            if attributes.and_then(|policy| policy.secure) == Some(false) {
                continue;
            }
        } else if name.eq_ignore_ascii_case("httponly") {
            saw_http_only = true;
            if attributes.and_then(|policy| policy.http_only) == Some(false) {
                continue;
            }
        } else if name.eq_ignore_ascii_case("samesite") {
            saw_same_site = true;
            if let Some(same_site) = attributes.and_then(|policy| policy.same_site) {
                append_cookie_attribute(
                    &mut rewritten,
                    whitespace,
                    "SameSite",
                    Some(same_site_value(same_site)),
                );
                continue;
            }
        }
        if let Some(value) = value {
            let value = if name.eq_ignore_ascii_case("path") {
                rewrite_cookie_path(&policy.cookie_path_rewrites, value)
            } else {
                Cow::Borrowed(value)
            };
            append_cookie_attribute(&mut rewritten, whitespace, name, Some(&value));
        } else {
            append_cookie_attribute(&mut rewritten, whitespace, trimmed, None);
        }
    }
    if let Some(attributes) = attributes {
        if attributes.secure == Some(true) && !saw_secure {
            append_cookie_attribute(&mut rewritten, " ", "Secure", None);
        }
        if attributes.http_only == Some(true) && !saw_http_only {
            append_cookie_attribute(&mut rewritten, " ", "HttpOnly", None);
        }
        if let Some(same_site) = attributes.same_site.filter(|_| !saw_same_site) {
            append_cookie_attribute(
                &mut rewritten,
                " ",
                "SameSite",
                Some(same_site_value(same_site)),
            );
        }
    }
    HeaderValue::from_str(&rewritten).map_err(|_| ResponsePolicyError::InvalidCookie)
}

/// The first rewrite that matches replaces the value; `nginx_prefix` splices `to` in front of what follows `from`.
fn rewrite_cookie_path<'a>(rewrites: &'a [HttpCookiePathRewrite], value: &'a str) -> Cow<'a, str> {
    rewrites
        .iter()
        .find_map(|rewrite| match rewrite.mode {
            CookiePathMatch::Exact => {
                (rewrite.from == value).then_some(Cow::Borrowed(rewrite.to.as_str()))
            }
            CookiePathMatch::NginxPrefix => value
                .strip_prefix(rewrite.from.as_str())
                .map(|rest| Cow::Owned(format!("{}{rest}", rewrite.to))),
        })
        .unwrap_or(Cow::Borrowed(value))
}

fn append_cookie_attribute(output: &mut String, whitespace: &str, name: &str, value: Option<&str>) {
    output.push(';');
    output.push_str(whitespace);
    output.push_str(name);
    if let Some(value) = value {
        output.push('=');
        output.push_str(value);
    }
}

const fn same_site_value(value: HttpSameSite) -> &'static str {
    match value {
        HttpSameSite::Strict => "Strict",
        HttpSameSite::Lax => "Lax",
        HttpSameSite::None => "None",
    }
}

pub(crate) fn normalized_request_host(authority: &Authority) -> Cow<'_, str> {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    match unbracketed.parse::<IpAddr>() {
        Ok(ip) => Cow::Owned(ip.to_string()),
        Err(_) if host.bytes().any(|byte| byte.is_ascii_uppercase()) => {
            Cow::Owned(host.to_ascii_lowercase())
        }
        Err(_) => Cow::Borrowed(host),
    }
}

/// Appends the host nginx sends for `authority` to `out`: lowercased without a trailing dot, an IPv4 address as
/// text and an IPv6 one in brackets; `fallback` when the request has no authority.
fn render_nginx_host(authority: Option<&Authority>, fallback: &HeaderValue, out: &mut Vec<u8>) {
    let Some(authority) = authority else {
        out.extend_from_slice(fallback.as_bytes());
        return;
    };
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    match unbracketed.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => out.extend_from_slice(IpText::new(IpAddr::V4(ip)).as_bytes()),
        Ok(IpAddr::V6(ip)) => {
            std::io::Write::write_fmt(out, format_args!("[{ip}]")).expect("writing to a vec");
        }
        Err(_) => out.extend(
            host.strip_suffix('.')
                .unwrap_or(host)
                .bytes()
                .map(|byte| byte.to_ascii_lowercase()),
        ),
    }
}

pub(crate) fn nginx_request_host(
    authority: Option<&Authority>,
    fallback: &HeaderValue,
) -> Result<HeaderValue, RequestPolicyError> {
    if authority.is_none() {
        return Ok(fallback.clone());
    }
    let mut host = Vec::new();
    render_nginx_host(authority, fallback, &mut host);
    HeaderValue::from_bytes(&host).map_err(|_| RequestPolicyError::InvalidHeader)
}

#[derive(Clone, Copy)]
pub(crate) struct RedirectContext<'a> {
    pub(crate) scheme: &'a str,
    pub(crate) normalized_host: Option<&'a str>,
    pub(crate) request_uri: &'a str,
}

pub(crate) fn expand_redirect_location(
    location: &RedirectLocationPlan,
    context: RedirectContext<'_>,
) -> Option<HeaderValue> {
    let RedirectLocationPlan::RequestTemplate {
        segments,
        nginx_host_fallback,
    } = location
    else {
        let RedirectLocationPlan::Literal(value) = location else {
            unreachable!()
        };
        return Some(value.clone());
    };

    let host = context
        .normalized_host
        .or(nginx_host_fallback.as_deref())
        .unwrap_or_default();
    let mut expanded = String::new();
    for segment in segments {
        expanded.push_str(match segment {
            RedirectTemplateSegment::Literal(value) => value,
            RedirectTemplateSegment::Scheme => context.scheme,
            RedirectTemplateSegment::Host => host,
            RedirectTemplateSegment::RequestUri => context.request_uri,
        });
    }
    (expanded.len() <= MAX_REDIRECT_LOCATION_BYTES)
        .then(|| HeaderValue::from_str(&expanded).ok())
        .flatten()
}

pub(crate) fn normalized_redirect_host(authority: &Authority) -> String {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed.parse::<IpAddr>().map_or_else(
        |_| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase(),
        |ip| match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        },
    )
}

#[cfg(test)]
mod tests;
