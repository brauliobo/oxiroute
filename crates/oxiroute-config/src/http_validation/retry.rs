#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_retry(
    service: &str,
    route_index: usize,
    retry: &mut HttpRetryPolicy,
    route_policy: crate::model::HttpRoutePolicy,
) -> Result<(), ConfigError> {
    if retry.max_retries > MAX_HTTP_RETRIES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.max_retries",
            "must be between 0 and 3",
        ));
    }
    if retry.delay_ms > 60_000 {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.delay_ms",
            "retry delay must not exceed 60000 milliseconds",
        ));
    }
    if retry.jitter_percent > 100 {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.jitter_percent",
            "jitter must not exceed 100 percent",
        ));
    }
    validate_retry_idempotency_header(service, route_index, retry)?;
    validate_retry_budget(service, route_index, retry.budget)?;
    if retry.final_redispatch
        && (retry.max_retries == 0 || retry.target != crate::model::HttpRetryTarget::SameServer)
    {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.final_redispatch",
            "final redispatch requires at least one same-server retry",
        ));
    }
    validate_buffered_retry_safety(service, route_index, retry, route_policy)?;
    if retry.triggers.is_empty() && retry.max_retries != 0 && retry.response_statuses.is_empty() {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.triggers",
            "must not be empty when retries are enabled without response statuses",
        ));
    }
    let mut triggers = HashSet::with_capacity(retry.triggers.len());
    for trigger in &retry.triggers {
        if !triggers.insert(*trigger) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.retry.triggers",
                "must not contain duplicates",
            ));
        }
    }
    retry
        .triggers
        .sort_unstable_by_key(|trigger| match trigger {
            HttpRetryTrigger::ConnectFailure => 0,
            HttpRetryTrigger::ConnectTimeout => 1,
            HttpRetryTrigger::RefusedStream => 2,
            HttpRetryTrigger::EmptyResponse => 3,
            HttpRetryTrigger::ResponseTimeout => 4,
            HttpRetryTrigger::JunkResponse => 5,
        });
    if retry.response_statuses.len() > crate::defaults::MAX_HTTP_RETRY_RESPONSE_STATUSES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.response_statuses",
            "must contain at most 100 statuses",
        ));
    }
    let mut response_statuses = HashSet::with_capacity(retry.response_statuses.len());
    for status in &retry.response_statuses {
        if !(100..=599).contains(status) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.retry.response_statuses",
                "must contain only statuses between 100 and 599",
            ));
        }
        if !response_statuses.insert(*status) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.retry.response_statuses",
                "must not contain duplicates",
            ));
        }
    }
    retry.response_statuses.sort_unstable();
    Ok(())
}

fn validate_retry_idempotency_header(
    service: &str,
    route_index: usize,
    retry: &mut HttpRetryPolicy,
) -> Result<(), ConfigError> {
    let Some(header) = &mut retry.idempotency_header else {
        return Ok(());
    };
    normalize_header_name(header).map_err(|detail| {
        invalid_route(
            service,
            route_index,
            "action.policy.retry.idempotency_header",
            detail,
        )
    })?;
    if retry.method_safety != crate::model::HttpRetryMethodSafety::All {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.idempotency_header",
            "requires method_safety = \"all\"",
        ));
    }
    Ok(())
}

fn validate_retry_budget(
    service: &str,
    route_index: usize,
    budget: Option<crate::model::HttpRetryBudget>,
) -> Result<(), ConfigError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let invalid =
        |detail| invalid_route(service, route_index, "action.policy.retry.budget", detail);
    if budget.per_second.is_none() && budget.ratio_percent.is_none() {
        return Err(invalid("requires per_second or ratio_percent"));
    }
    if budget
        .per_second
        .is_some_and(|rate| !(1..=100_000).contains(&rate))
    {
        return Err(invalid("per_second must be between 1 and 100000"));
    }
    if budget
        .ratio_percent
        .is_some_and(|ratio| !(1..=100).contains(&ratio))
    {
        return Err(invalid("ratio_percent must be between 1 and 100"));
    }
    Ok(())
}

fn validate_buffered_retry_safety(
    service: &str,
    route_index: usize,
    retry: &HttpRetryPolicy,
    route_policy: crate::model::HttpRoutePolicy,
) -> Result<(), ConfigError> {
    let all_methods = retry.method_safety == crate::model::HttpRetryMethodSafety::All;
    let buffered_body = retry.body_safety == crate::model::HttpRetryBodySafety::Buffered;
    if all_methods != buffered_body {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry",
            "all-method retries require buffered request bodies, and buffered request bodies require all-method retries",
        ));
    }
    if buffered_body && !route_policy.request_buffering {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.body_safety",
            "buffered request-body retries require request_buffering",
        ));
    }
    if buffered_body && route_policy.max_request_body_bytes.is_none() {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.retry.body_safety",
            "buffered request-body retries require a positive max_request_body_bytes bound",
        ));
    }
    Ok(())
}
