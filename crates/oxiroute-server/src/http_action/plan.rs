#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct HttpRoutePlan {
    pub(crate) access: Option<RouteAccess>,
    pub(crate) action: HttpActionPlan,
    pub(crate) policy: RoutePolicyPlan,
    pub(crate) route_id: String,
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

#[derive(Debug)]
pub(crate) struct ProxyActionPlan {
    pub(crate) pool: Arc<UpstreamPlan>,
    pub(crate) rescue_pool: Option<Arc<UpstreamPlan>>,
    pub(crate) policy: ProxyPolicyPlan,
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
}

impl FixedResponsePlan {
    pub(crate) fn compile(status: u16, body: &str, headers: &[HttpLiteralHeader]) -> Self {
        Self {
            status,
            body: Bytes::copy_from_slice(body.as_bytes()),
            headers: headers
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
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RedirectPlan {
    pub(crate) status: u16,
    pub(crate) location: RedirectLocationPlan,
    pub(crate) headers: Box<[(HeaderName, HeaderValue)]>,
}

impl RedirectPlan {
    pub(crate) fn compile(
        status: u16,
        location: &HttpRedirectLocation,
        headers: &[HttpLiteralHeader],
    ) -> Self {
        Self {
            status,
            location: RedirectLocationPlan::compile(location)
                .expect("validated redirect location template"),
            headers: headers
                .iter()
                .filter(|header| header.always || nginx_add_header_status(status))
                .map(|header| {
                    (
                        HeaderName::from_bytes(header.name.as_bytes())
                            .expect("validated redirect header name"),
                        HeaderValue::from_str(&header.value)
                            .expect("validated redirect header value"),
                    )
                })
                .collect(),
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
