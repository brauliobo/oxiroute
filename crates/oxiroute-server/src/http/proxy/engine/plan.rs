use crate::http::{action::HttpActionPlan, proxy::lane::LaneRoute};

pub(crate) fn serves(lane: LaneRoute, action: &HttpActionPlan) -> bool {
    let HttpActionPlan::Proxy(proxy) = action else {
        return false;
    };
    let pool = &proxy.pool;
    lane == LaneRoute::Proxy
        && pool.tls().is_none()
        && pool.selector().selects_at_once()
        && pool.selector().reaches_fixed_addresses()
        && proxy.policy.retries_without_delay()
}
