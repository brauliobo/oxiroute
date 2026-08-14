use crate::planning_errors::ServicePlanError;

pub(crate) fn reject_unimplemented_runtime_policies(
    config: &oxiroute_config::ConfigDraft,
) -> Result<(), ServicePlanError> {
    let unavailable = |policy| ServicePlanError::RuntimePolicyUnavailable { policy };
    for service in &config.http_services {
        for route in &service.routes {
            if route.policy.response_buffering && route.policy.max_request_body_bytes.is_none() {
                return Err(unavailable(
                    "http_services[].routes[].policy.unbounded_response_buffering",
                ));
            }
            if route.policy.request_buffering && route.policy.max_request_body_bytes.is_none() {
                return Err(unavailable(
                    "http_services[].routes[].policy.unbounded_request_buffering",
                ));
            }
        }
    }
    Ok(())
}
