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
    pub(crate) retry_jitter_percent: u8,
    pub(crate) idempotency_header: Option<HeaderName>,
    pub(crate) retry_budget: Option<Arc<RetryBudget>>,
    pub(crate) final_redispatch: bool,
    pub(crate) timeouts: HttpProxyTimeouts,
    pub(crate) rescue_statuses: Box<[u16]>,
    pub(crate) cache: Option<Arc<HttpCachePlan>>,
    pub(crate) affinity: Option<AffinityPlan>,
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
            retry_jitter_percent: self.retry_jitter_percent,
            idempotency_header: self.idempotency_header.clone(),
            retry_budget: self.retry_budget.clone(),
            final_redispatch: self.final_redispatch,
            timeouts: self.timeouts,
            rescue_statuses: self.rescue_statuses.clone(),
            cache,
            affinity: self.affinity.clone(),
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
            retry_jitter_percent: policy.retry.jitter_percent,
            idempotency_header: policy.retry.idempotency_header.as_deref().map(|name| {
                HeaderName::from_bytes(name.as_bytes()).expect("validated idempotency header")
            }),
            retry_budget: policy
                .retry
                .budget
                .map(|budget| Arc::new(RetryBudget::new(budget))),
            final_redispatch: policy.retry.final_redispatch,
            timeouts: policy.timeouts,
            rescue_statuses: policy
                .rescue
                .as_ref()
                .map_or_else(Box::default, |rescue| rescue.statuses.clone().into()),
            cache,
            affinity: policy.affinity.as_ref().map(AffinityPlan::compile),
            nginx_error_server,
        }
    }

    pub(crate) fn retries_on(&self, trigger: HttpRetryTrigger) -> bool {
        self.retry_triggers.contains(&trigger)
    }

    pub(crate) fn request_is_replay_safe(
        &self,
        method: &Method,
        headers: &HeaderMap,
        body_is_empty: bool,
        request_buffered: bool,
    ) -> bool {
        let method_safe = match self.retry_method_safety {
            HttpRetryMethodSafety::GetHead => matches!(*method, Method::GET | Method::HEAD),
            HttpRetryMethodSafety::All => {
                matches!(*method, Method::GET | Method::HEAD)
                    || self
                        .idempotency_header
                        .as_ref()
                        .is_none_or(|name| headers.get(name).is_some_and(|value| !value.is_empty()))
            }
        };
        let body_safe = body_is_empty
            || (self.retry_body_safety == HttpRetryBodySafety::Buffered && request_buffered);
        method_safe && body_safe
    }

    pub(crate) fn retries_on_status(&self, status: u16) -> bool {
        self.retry_response_statuses.contains(&status)
    }

    pub(crate) fn rescues_status(&self, status: u16) -> bool {
        self.rescue_statuses.contains(&status)
    }

    /// The delay before a retry: `retry_delay` plus up to `retry_jitter_percent` of it.
    pub(crate) fn next_retry_delay(&self) -> Duration {
        jittered_delay(self.retry_delay, self.retry_jitter_percent, random_unit())
    }

    pub(crate) fn target_for_retry(&self, attempts: usize) -> HttpRetryTarget {
        if self.final_redispatch && attempts == usize::from(self.max_retries) {
            HttpRetryTarget::NextServer
        } else {
            self.retry_target
        }
    }
}

pub(super) fn jittered_delay(delay: Duration, jitter_percent: u8, unit: f64) -> Duration {
    delay.mul_f64(unit.mul_add(f64::from(jitter_percent) / 100.0, 1.0))
}

pub(super) fn random_unit() -> f64 {
    use std::hash::{BuildHasher as _, Hasher as _, RandomState};
    let bits = RandomState::new().build_hasher().finish() >> 11;
    bits as f64 / (1_u64 << 53) as f64
}

/// Caps the retries of a route per second and as a ratio of its recent requests, so that retries
/// cannot amplify an outage. Windows are fixed: one second for the rate, ten for the ratio.
#[derive(Debug)]
pub(crate) struct RetryBudget {
    per_second: Option<u32>,
    ratio_percent: Option<u8>,
    window: Mutex<BudgetWindow>,
}

#[derive(Debug)]
struct BudgetWindow {
    second_started: Instant,
    second_retries: u32,
    ratio_started: Instant,
    requests: u64,
    retries: u64,
}

pub(super) const BUDGET_RATIO_WINDOW: Duration = Duration::from_secs(10);

impl RetryBudget {
    pub(crate) fn new(budget: oxiroute_config::HttpRetryBudget) -> Self {
        let now = Instant::now();
        Self {
            per_second: budget.per_second,
            ratio_percent: budget.ratio_percent,
            window: Mutex::new(BudgetWindow {
                second_started: now,
                second_retries: 0,
                ratio_started: now,
                requests: 0,
                retries: 0,
            }),
        }
    }

    pub(crate) fn record_request(&self, now: Instant) {
        let mut window = self.rolled_window(now);
        window.requests += 1;
    }

    /// Takes one retry from the budget, or returns `false` when the route spent it.
    pub(crate) fn try_spend(&self, now: Instant) -> bool {
        let mut window = self.rolled_window(now);
        let within_rate = self
            .per_second
            .is_none_or(|rate| window.second_retries < rate);
        let within_ratio = self.ratio_percent.is_none_or(|ratio| {
            window.retries < (window.requests * u64::from(ratio)).div_ceil(100)
        });
        if within_rate && within_ratio {
            window.second_retries += 1;
            window.retries += 1;
        }
        within_rate && within_ratio
    }

    fn rolled_window(&self, now: Instant) -> std::sync::MutexGuard<'_, BudgetWindow> {
        let mut window = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.duration_since(window.second_started) >= Duration::from_secs(1) {
            window.second_started = now;
            window.second_retries = 0;
        }
        if now.duration_since(window.ratio_started) >= BUDGET_RATIO_WINDOW {
            window.ratio_started = now;
            window.requests = 0;
            window.retries = 0;
        }
        window
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
