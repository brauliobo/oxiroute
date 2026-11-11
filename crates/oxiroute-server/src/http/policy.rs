use std::{borrow::Cow, net::IpAddr};

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::SET_COOKIE, uri::Authority};
use oxiroute_config::HttpSameSite;

use crate::http::action::{
    MAX_REDIRECT_LOCATION_BYTES, ProxyPolicyPlan, RedirectLocationPlan, RedirectTemplateSegment,
    RequestHeaderMutationPlan, RequestHeaderValuePlan, ResponseHeaderMutationPlan,
};
use crate::http::hop_by_hop::{has_hop_by_hop, hop_by_hop_removals};

/// The headers of the request a policy reads: a parsed map, or the scanned head of the fast lane.
pub(crate) trait IncomingHeaders {
    /// The values of every header called `name`, joined by `, ` in arrival order; `None` when there are none.
    fn joined(
        &self,
        name: &HeaderName,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RequestPolicyError>;
}

impl IncomingHeaders for HeaderMap {
    fn joined(
        &self,
        name: &HeaderName,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RequestPolicyError> {
        join_values(
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

pub(crate) fn decide_request_header<H: IncomingHeaders + ?Sized>(
    mutation: &RequestHeaderMutationPlan,
    context: RequestPolicyContext<'_, H>,
) -> Result<RequestHeaderDecision, RequestPolicyError> {
    let (name, value) = match mutation {
        RequestHeaderMutationPlan::Set { name, value } => (name, value),
        RequestHeaderMutationPlan::Remove { name } => {
            return Ok(RequestHeaderDecision::Remove(name.clone()));
        }
    };
    let value = match value {
        RequestHeaderValuePlan::Literal(value) => RequestHeaderValueDecision::Value(value.clone()),
        RequestHeaderValuePlan::IncomingAuthority => RequestHeaderValueDecision::Value(
            header_value(context.authority.map(Authority::as_str).unwrap_or_default())?,
        ),
        RequestHeaderValuePlan::NormalizedHost => {
            let host = context
                .authority
                .map(normalized_request_host)
                .unwrap_or_default();
            RequestHeaderValueDecision::Value(header_value(&host)?)
        }
        RequestHeaderValuePlan::NginxHost { fallback } => {
            RequestHeaderValueDecision::Value(nginx_request_host(context.authority, fallback)?)
        }
        RequestHeaderValuePlan::ClientIp => RequestHeaderValueDecision::ClientIp(
            context
                .client_ip
                .map(|client_ip| {
                    bounded_header_value(IpText::new(client_ip).as_bytes(), usize::MAX)
                })
                .transpose()?,
        ),
        RequestHeaderValuePlan::AppendedXForwardedFor {
            max_bytes,
            except_source_cidrs,
        } => RequestHeaderValueDecision::XForwardedFor(decide_x_forwarded_for(
            context,
            *max_bytes,
            except_source_cidrs,
        )?),
        RequestHeaderValuePlan::DownstreamScheme => {
            RequestHeaderValueDecision::Value(HeaderValue::from_static(context.downstream_scheme))
        }
        RequestHeaderValuePlan::IncomingHeader { name, max_bytes } => {
            let value = context
                .incoming_headers
                .joined(name, *max_bytes)?
                .unwrap_or_default();
            RequestHeaderValueDecision::Value(bounded_header_value(&value, *max_bytes)?)
        }
        RequestHeaderValuePlan::SelectedUpstreamHost => {
            RequestHeaderValueDecision::SelectedUpstreamHost
        }
    };
    Ok(RequestHeaderDecision::Set {
        name: name.clone(),
        value,
    })
}

fn decide_x_forwarded_for<H: IncomingHeaders + ?Sized>(
    context: RequestPolicyContext<'_, H>,
    max_bytes: usize,
    exceptions: &[crate::http::action::SourceCidr],
) -> Result<XForwardedForDecision, RequestPolicyError> {
    if context.client_ip.is_some_and(|client_ip| {
        exceptions
            .iter()
            .any(|exception| exception.contains(client_ip))
    }) {
        return Ok(XForwardedForDecision::Preserve);
    }
    let name = HeaderName::from_static("x-forwarded-for");
    let existing = context.incoming_headers.joined(&name, max_bytes)?;
    let Some(client_ip) = context.client_ip else {
        return existing.map_or(Ok(XForwardedForDecision::NoOp), |value| {
            bounded_header_value(&value, max_bytes).map(XForwardedForDecision::Set)
        });
    };
    let client_ip = IpText::new(client_ip);
    let Some(mut value) = existing else {
        return bounded_header_value(client_ip.as_bytes(), max_bytes)
            .map(XForwardedForDecision::Set);
    };
    extend_bounded(&mut value, b", ", max_bytes)?;
    extend_bounded(&mut value, client_ip.as_bytes(), max_bytes)?;
    bounded_header_value(&value, max_bytes).map(XForwardedForDecision::Set)
}

/// The text of an address in a stack buffer: no allocation, and IPv4 without the formatter.
struct IpText {
    buf: [u8; 45],
    len: usize,
}

impl IpText {
    fn new(address: IpAddr) -> Self {
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

    fn push(&mut self, byte: u8) {
        self.buf[self.len] = byte;
        self.len += 1;
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

pub(crate) fn join_values<'v>(
    values: impl Iterator<Item = &'v [u8]>,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, RequestPolicyError> {
    let mut joined = Vec::new();
    for value in values {
        if !joined.is_empty() {
            extend_bounded(&mut joined, b", ", max_bytes)?;
        }
        extend_bounded(&mut joined, value, max_bytes)?;
    }
    Ok((!joined.is_empty()).then_some(joined))
}

fn extend_bounded(
    output: &mut Vec<u8>,
    value: &[u8],
    max_bytes: usize,
) -> Result<(), RequestPolicyError> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|length| length > max_bytes)
    {
        return Err(RequestPolicyError::SourceTooLarge);
    }
    output.extend_from_slice(value);
    Ok(())
}

fn bounded_header_value(value: &[u8], max_bytes: usize) -> Result<HeaderValue, RequestPolicyError> {
    if value.len() > max_bytes {
        return Err(RequestPolicyError::SourceTooLarge);
    }
    HeaderValue::from_bytes(value).map_err(|_| RequestPolicyError::InvalidHeader)
}

fn header_value(value: &str) -> Result<HeaderValue, RequestPolicyError> {
    HeaderValue::from_str(value).map_err(|_| RequestPolicyError::InvalidHeader)
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
            let replacement = (name.eq_ignore_ascii_case("path"))
                .then(|| {
                    policy
                        .cookie_path_rewrites
                        .iter()
                        .find(|rewrite| rewrite.from == value)
                })
                .flatten();
            append_cookie_attribute(
                &mut rewritten,
                whitespace,
                name,
                Some(replacement.map_or(value, |replacement| replacement.to.as_str())),
            );
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

pub(crate) fn nginx_request_host(
    authority: Option<&Authority>,
    fallback: &HeaderValue,
) -> Result<HeaderValue, RequestPolicyError> {
    let Some(authority) = authority else {
        return Ok(fallback.clone());
    };
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let normalized = unbracketed.parse::<IpAddr>().map_or_else(
        |_| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase(),
        |ip| match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        },
    );
    header_value(&normalized)
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
