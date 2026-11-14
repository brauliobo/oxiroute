use std::{collections::BTreeSet, fmt};

use crate::{
    HttpProxyPolicy, HttpRetryMethodSafety, HttpRetryPolicy, HttpRetryTrigger, HttpRoute,
    HttpRouteAction, UpstreamTlsVerify, ValidatedConfig,
};

/// Response-phase retries are configured on a route that accepts no method replay safety allows.
pub const W_RETRY_NEVER_REPLAYS: &str = "W_RETRY_NEVER_REPLAYS";

/// A pool serves status-based retries but never ejects a server that keeps returning those statuses.
pub const W_PASSIVE_HEALTH_MISSING: &str = "W_PASSIVE_HEALTH_MISSING";

/// A pool encrypts to its origin but accepts any certificate, so it does not authenticate the origin.
pub const W_UPSTREAM_TLS_UNVERIFIED: &str = "W_UPSTREAM_TLS_UNVERIFIED";

/// A valid setting that cannot behave the way it reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigWarning {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl ValidatedConfig {
    /// Settings that validate but cannot do what they say; they never change the effective policy.
    #[must_use]
    pub fn warnings(&self) -> Vec<ConfigWarning> {
        let config = self.as_draft();
        let mut warnings = Vec::new();
        let mut status_retry_pools = BTreeSet::new();
        for service in &config.http_services {
            for (index, route) in service.routes.iter().enumerate() {
                let HttpRouteAction::Proxy {
                    upstream_pool,
                    policy,
                } = &route.action
                else {
                    continue;
                };
                if never_replays(route, policy) {
                    warnings.push(ConfigWarning {
                        code: W_RETRY_NEVER_REPLAYS,
                        message: format!(
                            "HTTP service `{}` route {index} retries after the request was sent, but accepts only methods other than GET and HEAD, which are the only methods replayed by default; opt in to buffered all-method retries or drop the response-based retries",
                            service.name
                        ),
                    });
                }
                if policy.retry.retries_on_statuses() {
                    status_retry_pools.extend(served_pools(upstream_pool, policy));
                }
            }
        }
        warnings.extend(
            config
                .upstream_pools
                .iter()
                .filter(|pool| {
                    pool.passive_health.is_none() && status_retry_pools.contains(pool.name.as_str())
                })
                .map(|pool| ConfigWarning {
                    code: W_PASSIVE_HEALTH_MISSING,
                    message: format!(
                        "upstream pool `{}` serves routes that retry on response statuses but has no `passive_health`, so a server that keeps returning them is never ejected",
                        pool.name
                    ),
                }),
        );
        warnings.extend(
            config
                .upstream_pools
                .iter()
                .filter(|pool| pool.tls.as_ref().is_some_and(|tls| tls.verify == UpstreamTlsVerify::None))
                .map(|pool| ConfigWarning {
                    code: W_UPSTREAM_TLS_UNVERIFIED,
                    message: format!(
                        "upstream pool `{}` uses tls with verify none, so any certificate is accepted and the origin is not authenticated",
                        pool.name
                    ),
                }),
        );
        warnings
    }
}

impl HttpRetryPolicy {
    fn retries_on_statuses(&self) -> bool {
        self.max_retries > 0 && !self.response_statuses.is_empty()
    }

    /// Retries that happen after the request was sent, so they depend on replay safety.
    fn retries_after_send(&self) -> bool {
        self.retries_on_statuses()
            || (self.max_retries > 0
                && self.triggers.iter().any(|trigger| {
                    matches!(
                        trigger,
                        HttpRetryTrigger::EmptyResponse
                            | HttpRetryTrigger::ResponseTimeout
                            | HttpRetryTrigger::JunkResponse
                    )
                }))
    }
}

fn never_replays(route: &HttpRoute, policy: &HttpProxyPolicy) -> bool {
    policy.retry.method_safety == HttpRetryMethodSafety::GetHead
        && (policy.retry.retries_after_send()
            || (policy.retry.max_retries > 0 && policy.rescue.is_some()))
        && !route.methods.is_empty()
        && !route
            .methods
            .iter()
            .any(|method| matches!(method.as_str(), "GET" | "HEAD"))
}

fn served_pools<'a>(
    upstream_pool: &'a str,
    policy: &'a HttpProxyPolicy,
) -> impl Iterator<Item = &'a str> {
    let selected = policy.select.iter().flat_map(|select| &select.rules);
    std::iter::once(upstream_pool).chain(selected.map(|rule| rule.pool.as_str()))
}
