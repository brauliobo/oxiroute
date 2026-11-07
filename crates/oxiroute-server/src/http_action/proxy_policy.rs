#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct ProxyPolicyPlan {
    pub(crate) upstream_host: HttpUpstreamHost,
    pub(crate) upstream_path_rewrite: Option<HttpProxyPathRewrite>,
    pub(crate) request_headers: Box<[RequestHeaderMutationPlan]>,
    pub(crate) response_headers: Box<[ResponseHeaderMutationPlan]>,
    pub(crate) cookie_path_rewrites: Box<[HttpCookiePathRewrite]>,
    pub(crate) cookie_attributes: Box<[HttpCookieAttributePolicy]>,
    pub(crate) max_retries: u8,
    pub(crate) retry_triggers: Box<[HttpRetryTrigger]>,
    pub(crate) retry_response_statuses: Box<[u16]>,
    pub(crate) retry_method_safety: HttpRetryMethodSafety,
    pub(crate) retry_body_safety: HttpRetryBodySafety,
    pub(crate) retry_target: HttpRetryTarget,
    pub(crate) retry_delay: Duration,
    pub(crate) final_redispatch: bool,
    pub(crate) cache: Option<Arc<HttpCachePlan>>,
    pub(crate) nginx_error_server: Option<HeaderValue>,
}

impl ProxyPolicyPlan {
    pub(crate) fn compile(policy: &HttpProxyPolicy) -> Self {
        Self::compile_with_cache(policy, None)
    }

    pub(crate) fn clone_with_cache(&self, cache: Option<Arc<HttpCachePlan>>) -> Self {
        Self {
            upstream_host: self.upstream_host.clone(),
            upstream_path_rewrite: self.upstream_path_rewrite.clone(),
            request_headers: self
                .request_headers
                .iter()
                .map(RequestHeaderMutationPlan::duplicate)
                .collect(),
            response_headers: self.response_headers.clone(),
            cookie_path_rewrites: self.cookie_path_rewrites.clone(),
            cookie_attributes: self.cookie_attributes.clone(),
            max_retries: self.max_retries,
            retry_triggers: self.retry_triggers.clone(),
            retry_response_statuses: self.retry_response_statuses.clone(),
            retry_method_safety: self.retry_method_safety,
            retry_body_safety: self.retry_body_safety,
            retry_target: self.retry_target,
            retry_delay: self.retry_delay,
            final_redispatch: self.final_redispatch,
            cache,
            nginx_error_server: self.nginx_error_server.clone(),
        }
    }

    pub(crate) fn compile_with_cache(
        policy: &HttpProxyPolicy,
        cache: Option<Arc<HttpCachePlan>>,
    ) -> Self {
        let nginx_content_type_marker = |mutation: &HttpResponseHeaderMutation| {
            matches!(
                mutation,
                HttpResponseHeaderMutation::Set {
                    name,
                    value,
                    always: true,
                } if name.eq_ignore_ascii_case("content-type")
                    && value.eq_ignore_ascii_case("text/html")
            )
        };
        let nginx_error_server = policy
            .response_headers
            .iter()
            .any(nginx_content_type_marker)
            .then(|| {
                policy.response_headers.iter().rev().find_map(|mutation| {
                    let HttpResponseHeaderMutation::Set {
                        name,
                        value,
                        always: true,
                    } = mutation
                    else {
                        return None;
                    };
                    name.eq_ignore_ascii_case("server").then(|| {
                        HeaderValue::from_str(value).expect("validated nginx server marker")
                    })
                })
            })
            .flatten();
        Self {
            upstream_host: policy.upstream_host.clone(),
            upstream_path_rewrite: policy.upstream_path_rewrite.clone(),
            request_headers: policy
                .request_headers
                .iter()
                .map(RequestHeaderMutationPlan::compile)
                .collect(),
            response_headers: policy
                .response_headers
                .iter()
                .filter(|mutation| {
                    nginx_error_server.is_none() || !nginx_content_type_marker(mutation)
                })
                .map(ResponseHeaderMutationPlan::compile)
                .collect(),
            cookie_path_rewrites: policy
                .response_cookie_path_rewrites
                .clone()
                .into_boxed_slice(),
            cookie_attributes: policy.response_cookie_attributes.clone().into_boxed_slice(),
            max_retries: policy.retry.max_retries,
            retry_triggers: policy.retry.triggers.clone().into_boxed_slice(),
            retry_response_statuses: policy.retry.response_statuses.clone().into_boxed_slice(),
            retry_method_safety: policy.retry.method_safety,
            retry_body_safety: policy.retry.body_safety,
            retry_target: policy.retry.target,
            retry_delay: Duration::from_millis(policy.retry.delay_ms),
            final_redispatch: policy.retry.final_redispatch,
            cache,
            nginx_error_server,
        }
    }

    pub(crate) fn retries_on(&self, trigger: HttpRetryTrigger) -> bool {
        self.retry_triggers.contains(&trigger)
    }

    pub(crate) fn request_is_replay_safe(
        &self,
        method: &Method,
        body_is_empty: bool,
        request_buffered: bool,
    ) -> bool {
        let method_safe = match self.retry_method_safety {
            HttpRetryMethodSafety::GetHead => matches!(*method, Method::GET | Method::HEAD),
            HttpRetryMethodSafety::All => true,
        };
        let body_safe = body_is_empty
            || (self.retry_body_safety == HttpRetryBodySafety::Buffered && request_buffered);
        method_safe && body_safe
    }

    pub(crate) fn retries_on_status(&self, status: u16) -> bool {
        self.retry_response_statuses.contains(&status)
    }

    pub(crate) fn target_for_retry(&self, attempts: usize) -> HttpRetryTarget {
        if self.final_redispatch && attempts == usize::from(self.max_retries) {
            HttpRetryTarget::NextServer
        } else {
            self.retry_target
        }
    }
}

#[derive(Debug)]
pub(crate) enum RequestHeaderMutationPlan {
    Set {
        name: HeaderName,
        value: RequestHeaderValuePlan,
    },
    Remove {
        name: HeaderName,
    },
}

impl RequestHeaderMutationPlan {
    fn duplicate(&self) -> Self {
        match self {
            Self::Set { name, value } => Self::Set {
                name: name.clone(),
                value: value.duplicate(),
            },
            Self::Remove { name } => Self::Remove { name: name.clone() },
        }
    }
    pub(crate) fn compile(mutation: &HttpRequestHeaderMutation) -> Self {
        match mutation {
            HttpRequestHeaderMutation::Set { name, value } => Self::Set {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated request header name"),
                value: RequestHeaderValuePlan::compile(value),
            },
            HttpRequestHeaderMutation::Remove { name } => Self::Remove {
                name: HeaderName::from_bytes(name.as_bytes())
                    .expect("validated request header name"),
            },
        }
    }

    pub(crate) fn is_pingora_managed_upgrade(&self) -> bool {
        matches!(
            self,
            Self::Set {
                name,
                value: RequestHeaderValuePlan::IncomingHeader { name: source, .. },
            } if name.as_str() == "upgrade" && source.as_str() == "upgrade"
        ) || matches!(
            self,
            Self::Set {
                name,
                value: RequestHeaderValuePlan::Literal(value),
            } if name.as_str() == "connection"
                && value.as_bytes().eq_ignore_ascii_case(b"upgrade")
        )
    }
}

#[derive(Debug)]
pub(crate) enum RequestHeaderValuePlan {
    Literal(HeaderValue),
    IncomingAuthority,
    NormalizedHost,
    NginxHost {
        fallback: HeaderValue,
    },
    ClientIp,
    AppendedXForwardedFor {
        max_bytes: usize,
        except_source_cidrs: Box<[SourceCidr]>,
    },
    DownstreamScheme,
    IncomingHeader {
        name: HeaderName,
        max_bytes: usize,
    },
    SelectedUpstreamHost,
}

impl RequestHeaderValuePlan {
    fn duplicate(&self) -> Self {
        match self {
            Self::Literal(value) => Self::Literal(value.clone()),
            Self::IncomingAuthority => Self::IncomingAuthority,
            Self::NormalizedHost => Self::NormalizedHost,
            Self::NginxHost { fallback } => Self::NginxHost {
                fallback: fallback.clone(),
            },
            Self::ClientIp => Self::ClientIp,
            Self::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => Self::AppendedXForwardedFor {
                max_bytes: *max_bytes,
                except_source_cidrs: except_source_cidrs.clone(),
            },
            Self::DownstreamScheme => Self::DownstreamScheme,
            Self::IncomingHeader { name, max_bytes } => Self::IncomingHeader {
                name: name.clone(),
                max_bytes: *max_bytes,
            },
            Self::SelectedUpstreamHost => Self::SelectedUpstreamHost,
        }
    }
    fn compile(value: &HttpRequestHeaderValue) -> Self {
        match value {
            HttpRequestHeaderValue::Literal { value } => {
                Self::Literal(HeaderValue::from_str(value).expect("validated request header value"))
            }
            HttpRequestHeaderValue::IncomingAuthority => Self::IncomingAuthority,
            HttpRequestHeaderValue::NormalizedHost => Self::NormalizedHost,
            HttpRequestHeaderValue::NginxHost { fallback } => Self::NginxHost {
                fallback: HeaderValue::from_str(fallback).expect("validated nginx host fallback"),
            },
            HttpRequestHeaderValue::ClientIp => Self::ClientIp,
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => Self::AppendedXForwardedFor {
                max_bytes: usize::try_from(*max_bytes).expect("validated header bound"),
                except_source_cidrs: except_source_cidrs
                    .iter()
                    .map(|cidr| SourceCidr::parse(cidr).expect("validated source CIDR"))
                    .collect(),
            },
            HttpRequestHeaderValue::DownstreamScheme => Self::DownstreamScheme,
            HttpRequestHeaderValue::IncomingHeader { name, max_bytes } => Self::IncomingHeader {
                name: HeaderName::from_bytes(name.as_bytes()).expect("validated incoming header"),
                max_bytes: usize::try_from(*max_bytes).expect("validated header bound"),
            },
            HttpRequestHeaderValue::SelectedUpstreamHost => Self::SelectedUpstreamHost,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SourceCidr {
    network: std::net::IpAddr,
    prefix: u8,
}

impl SourceCidr {
    fn parse(value: &str) -> Option<Self> {
        let (network, prefix) = value.split_once('/')?;
        Some(Self {
            network: network.parse().ok()?,
            prefix: prefix.parse().ok()?,
        })
    }

    pub(crate) fn contains(&self, address: std::net::IpAddr) -> bool {
        match (self.network, address) {
            (std::net::IpAddr::V4(network), std::net::IpAddr::V4(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - u32::from(self.prefix))
                };
                u32::from(network) & mask == u32::from(address) & mask
            }
            (std::net::IpAddr::V6(network), std::net::IpAddr::V6(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - u32::from(self.prefix))
                };
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}
