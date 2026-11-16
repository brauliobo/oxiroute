use oxiroute_config::UpstreamConnectionReuse;

use crate::http::{
    action::{HttpActionPlan, RoutePolicyPlan},
    proxy::{attempt::AttemptBase, lane::LaneRoute},
};

/// What the epoll data plane works out once for a route it serves, so that a request does none of it.
#[derive(Debug)]
pub(crate) struct EnginePlan {
    pub(in crate::http::proxy) attempt: AttemptBase,
    pub(in crate::http::proxy) connection_close: bool,
}

/// The plan of the route when the epoll data plane serves it.
pub(crate) fn plan(
    lane: LaneRoute,
    action: &HttpActionPlan,
    route: RoutePolicyPlan,
) -> Option<EnginePlan> {
    let HttpActionPlan::Proxy(proxy) = action else {
        return None;
    };
    let pool = &proxy.pool;
    let serves = lane == LaneRoute::Proxy
        && pool.tls().is_none()
        && pool.selector().selects_at_once()
        && pool.selector().reaches_fixed_addresses()
        && proxy.policy.retries_without_delay();
    serves.then(|| EnginePlan {
        attempt: AttemptBase::of(route, proxy.policy.timeouts, pool),
        connection_close: pool.connection_reuse() == UpstreamConnectionReuse::Never,
    })
}
