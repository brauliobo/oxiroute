#[allow(clippy::wildcard_imports)]
use super::*;
use crate::http::proxy::lane::LaneRoute;
use crate::http::request_id::REQUEST_ID_HEADER;
use crate::label::Label;
use http::header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, DATE, LOCATION, TRANSFER_ENCODING};

#[derive(Debug)]
pub(crate) struct HttpRoutePlan {
    pub(crate) access: Option<RouteAccess>,
    pub(crate) action: HttpActionPlan,
    pub(crate) lane: LaneRoute,
    pub(crate) policy: RoutePolicyPlan,
    pub(crate) route_id: Label,
}

#[derive(Clone, Debug)]
pub(crate) struct HttpGzipPlan {
    pub(crate) level: u32,
    pub(crate) content_types: Box<[String]>,
    pub(crate) min_length_bytes: usize,
    pub(crate) min_http_version: HttpGzipMinimumVersion,
    pub(crate) disable_on_via: bool,
    pub(crate) vary: bool,
}

impl HttpGzipPlan {
    pub(crate) fn compile(policy: &HttpGzipPolicy) -> Self {
        Self {
            level: u32::from(policy.level),
            content_types: policy.content_types.clone().into_boxed_slice(),
            min_length_bytes: usize::try_from(policy.min_length_bytes)
                .expect("validated gzip length fits usize"),
            min_http_version: policy.min_http_version,
            disable_on_via: policy.disable_on_via,
            vary: policy.vary,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RoutePolicyPlan {
    pub(crate) max_request_body_bytes: Option<u64>,
    pub(crate) request_buffering: bool,
    pub(crate) response_buffering: bool,
    pub(crate) connect_timeout: std::time::Duration,
    pub(crate) read_timeout: std::time::Duration,
    pub(crate) write_timeout: std::time::Duration,
}

impl RoutePolicyPlan {
    pub(crate) fn compile(policy: HttpRoutePolicy) -> Self {
        Self {
            max_request_body_bytes: policy.max_request_body_bytes,
            request_buffering: policy.request_buffering,
            response_buffering: policy.response_buffering,
            connect_timeout: std::time::Duration::from_millis(policy.connect_timeout_ms),
            read_timeout: std::time::Duration::from_millis(policy.read_timeout_ms),
            write_timeout: std::time::Duration::from_millis(policy.write_timeout_ms),
        }
    }

    pub(crate) fn exceeds_body_limit(self, bytes: u64) -> bool {
        self.max_request_body_bytes
            .is_some_and(|limit| bytes > limit)
    }

    pub(crate) fn request_body_buffer_limit(self) -> Option<usize> {
        self.request_buffering
            .then_some(self.max_request_body_bytes)
            .flatten()
            .and_then(|limit| usize::try_from(limit).ok())
    }

    pub(crate) fn response_body_buffer_limit(self) -> Option<usize> {
        self.response_buffering
            .then_some(self.max_request_body_bytes)
            .flatten()
            .and_then(|limit| usize::try_from(limit).ok())
    }
}

#[derive(Debug)]
pub(crate) enum HttpActionPlan {
    Proxy(ProxyActionPlan),
    Fixed(FixedResponsePlan),
    Redirect(RedirectPlan),
    Static(StaticFilesPlan),
}

impl HttpActionPlan {
    /// The response the action always writes, rendered once, for the fast lane.
    pub(crate) fn direct_response(&self) -> Option<&FixedResponsePlan> {
        match self {
            Self::Fixed(response) => Some(response),
            Self::Redirect(redirect) => redirect.direct.as_ref(),
            Self::Proxy(_) | Self::Static(_) => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProxyActionPlan {
    pub(crate) pool: Arc<UpstreamPlan>,
    pub(crate) rescue_pool: Option<Arc<UpstreamPlan>>,
    pub(crate) select: Option<PoolSelectPlan>,
    pub(crate) policy: Box<ProxyPolicyPlan>,
}

#[derive(Clone, Debug)]
pub(crate) enum ResponseHeaderMutationPlan {
    Set {
        name: HeaderName,
        value: HeaderValue,
        always: bool,
    },
    Add {
        name: HeaderName,
        value: HeaderValue,
        always: bool,
    },
    Remove {
        name: HeaderName,
    },
}

impl ResponseHeaderMutationPlan {
    /// Whether the mutation applies to a response with `status`: `add_header` without `always` skips the
    /// statuses nginx does.
    pub(crate) fn applies(&self, status: u16) -> bool {
        match self {
            Self::Set { always, .. } | Self::Add { always, .. } => {
                *always || nginx_add_header_status(status)
            }
            Self::Remove { .. } => true,
        }
    }

    pub(crate) fn compile(mutation: &HttpResponseHeaderMutation) -> Self {
        match mutation {
            HttpResponseHeaderMutation::Set {
                name,
                value,
                always,
            } => Self::Set {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
                value: HeaderValue::from_str(value).expect("validated response header value"),
                always: *always,
            },
            HttpResponseHeaderMutation::Add {
                name,
                value,
                always,
            } => Self::Add {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
                value: HeaderValue::from_str(value).expect("validated response header value"),
                always: *always,
            },
            HttpResponseHeaderMutation::Remove { name } => Self::Remove {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated response header name"),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FixedResponsePlan {
    pub(crate) status: u16,
    pub(crate) body: Bytes,
    pub(crate) headers: Box<[(HeaderName, HeaderValue)]>,
    /// Status line and header lines as an HTTP/1 response writes them, for the statuses and headers a
    /// response can be written from in one piece; `None` sends it through the generic response path.
    pub(crate) rendered_head: Option<Bytes>,
}

impl FixedResponsePlan {
    pub(crate) fn compile(status: u16, body: &str, headers: &[HttpLiteralHeader]) -> Self {
        let headers: Box<[(HeaderName, HeaderValue)]> = headers
            .iter()
            .filter(|header| header.always || nginx_add_header_status(status))
            .map(|header| {
                (
                    HeaderName::from_bytes(header.name.as_bytes())
                        .expect("validated fixed-response header name"),
                    HeaderValue::from_str(&header.value)
                        .expect("validated fixed-response header value"),
                )
            })
            .collect();
        Self::from_headers(status, Bytes::copy_from_slice(body.as_bytes()), headers)
    }

    fn from_headers(status: u16, body: Bytes, headers: Box<[(HeaderName, HeaderValue)]>) -> Self {
        Self {
            rendered_head: render_head(status, &headers, body.len()),
            status,
            body,
            headers,
        }
    }
}

/// The head of a response with a Content-Length body, as `write_rendered_response` expects it. The
/// headers a response writer owns (`Date`, `Connection`, the request id) and the ones that change the
/// body framing keep the generic path.
fn render_head(
    status: u16,
    headers: &[(HeaderName, HeaderValue)],
    body_length: usize,
) -> Option<Bytes> {
    let framed_by_length = status >= 200 && !matches!(status, 204 | 205 | 304);
    let owned = [DATE, CONNECTION, TRANSFER_ENCODING, REQUEST_ID_HEADER];
    if !framed_by_length || headers.iter().any(|(name, _)| owned.contains(name)) {
        return None;
    }
    let mut response =
        pingora::http::ResponseHeader::build_no_case(status, Some(headers.len() + 1)).ok()?;
    for (name, value) in headers {
        response.append_header(name.clone(), value.clone()).ok()?;
    }
    response
        .insert_header(CONTENT_LENGTH, body_length.to_string())
        .ok()?;
    let mut head = bytes::BytesMut::new();
    let reason = response.get_reason_phrase().unwrap_or_default();
    bytes::BufMut::put_slice(
        &mut head,
        format!("HTTP/1.1 {status} {reason}\r\n").as_bytes(),
    );
    response.header_to_h1_wire(&mut head);
    Some(head.freeze())
}

#[derive(Clone, Debug)]
pub(crate) struct RedirectPlan {
    pub(crate) status: u16,
    pub(crate) location: RedirectLocationPlan,
    pub(crate) headers: Box<[(HeaderName, HeaderValue)]>,
    /// The response of a redirect whose answer does not depend on the request: a literal location and no error
    /// page body (a `Content-Type: text/html` header asks for the nginx error page). It is what the fixed-response
    /// lane serves.
    pub(crate) direct: Option<FixedResponsePlan>,
}

impl RedirectPlan {
    pub(crate) fn compile(
        status: u16,
        location: &HttpRedirectLocation,
        headers: &[HttpLiteralHeader],
    ) -> Self {
        let location =
            RedirectLocationPlan::compile(location).expect("validated redirect location template");
        let headers: Box<[(HeaderName, HeaderValue)]> = headers
            .iter()
            .filter(|header| header.always || nginx_add_header_status(status))
            .map(|header| {
                (
                    HeaderName::from_bytes(header.name.as_bytes())
                        .expect("validated redirect header name"),
                    HeaderValue::from_str(&header.value).expect("validated redirect header value"),
                )
            })
            .collect();
        let direct = location.literal_header(&headers).map(|with_location| {
            FixedResponsePlan::from_headers(status, Bytes::new(), with_location)
        });
        Self {
            status,
            location,
            headers,
            direct,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RedirectLocationPlan {
    Literal(HeaderValue),
    RequestTemplate {
        segments: Box<[RedirectTemplateSegment]>,
        nginx_host_fallback: Option<Box<str>>,
    },
}

impl RedirectLocationPlan {
    /// `headers` followed by the `Location` of a literal redirect that has no error page body.
    fn literal_header(
        &self,
        headers: &[(HeaderName, HeaderValue)],
    ) -> Option<Box<[(HeaderName, HeaderValue)]>> {
        let Self::Literal(location) = self else {
            return None;
        };
        let error_page = headers.iter().any(|(name, value)| {
            name == CONTENT_TYPE && value.as_bytes().eq_ignore_ascii_case(b"text/html")
        });
        (!error_page).then(|| {
            let location = (LOCATION, location.clone());
            headers.iter().cloned().chain([location]).collect()
        })
    }

    pub(crate) fn compile(location: &HttpRedirectLocation) -> Option<Self> {
        match location {
            HttpRedirectLocation::Literal { value } => (value.len() <= MAX_REDIRECT_LOCATION_BYTES)
                .then(|| HeaderValue::from_str(value).ok())
                .flatten()
                .map(Self::Literal),
            HttpRedirectLocation::RequestTemplate {
                value,
                nginx_host_fallback,
            } => Some(Self::RequestTemplate {
                segments: compile_redirect_segments(value)?.into_boxed_slice(),
                nginx_host_fallback: nginx_host_fallback.as_deref().map(Into::into),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RedirectTemplateSegment {
    Literal(Box<str>),
    Scheme,
    Host,
    RequestUri,
}

pub(crate) const MAX_REDIRECT_LOCATION_BYTES: usize = 8 * 1024;

fn compile_redirect_segments(value: &str) -> Option<Vec<RedirectTemplateSegment>> {
    let mut segments = Vec::new();
    let mut remainder = value;
    while let Some((literal, variable)) = remainder.split_once('$') {
        if !literal.is_empty() {
            segments.push(RedirectTemplateSegment::Literal(literal.into()));
        }
        if let Some(after) = variable.strip_prefix("scheme") {
            segments.push(RedirectTemplateSegment::Scheme);
            remainder = after;
        } else if let Some(after) = variable.strip_prefix("host") {
            segments.push(RedirectTemplateSegment::Host);
            remainder = after;
        } else {
            let after = variable.strip_prefix("request_uri")?;
            segments.push(RedirectTemplateSegment::RequestUri);
            remainder = after;
        }
    }
    if !remainder.is_empty() {
        segments.push(RedirectTemplateSegment::Literal(remainder.into()));
    }
    Some(segments)
}
