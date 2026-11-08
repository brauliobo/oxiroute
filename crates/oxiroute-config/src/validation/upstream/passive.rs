#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_passive_health_config(
    pool: &str,
    policy: &crate::model::PassiveHealthPolicy,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidUpstreamServer {
        pool: pool.to_owned(),
        server: "<pool>".into(),
        field,
        detail,
    };
    if policy.error_limit == 0 || policy.error_limit > MAX_PASSIVE_ERROR_LIMIT {
        return Err(invalid(
            "passive_health.error_limit",
            "error limit must be between 1 and 100",
        ));
    }
    if policy.initial_backoff_ms == 0 {
        return Err(invalid(
            "passive_health.initial_backoff_ms",
            "initial backoff must be nonzero",
        ));
    }
    if policy.initial_backoff_ms > policy.max_backoff_ms {
        return Err(invalid(
            "passive_health.max_backoff_ms",
            "maximum backoff must not be shorter than the initial backoff",
        ));
    }
    if policy.max_backoff_ms > MAX_PASSIVE_BACKOFF_MS {
        return Err(invalid(
            "passive_health.max_backoff_ms",
            "maximum backoff must not exceed 86400000 milliseconds",
        ));
    }
    if policy.recovery_threshold == 0 || policy.recovery_threshold > MAX_PASSIVE_RECOVERY_THRESHOLD
    {
        return Err(invalid(
            "passive_health.recovery_threshold",
            "recovery threshold must be between 1 and 100",
        ));
    }
    if let Some(readmission) = &policy.readmission {
        validate_readmission(readmission, invalid)?;
    }
    if let Some(breaker) = &policy.circuit_breaker {
        validate_circuit_breaker(breaker, invalid)?;
    }
    if let Some(outlier) = &policy.outlier_detection {
        validate_outlier_detection(outlier, invalid)?;
    }
    Ok(())
}

pub(super) fn validate_outlier_detection(
    outlier: &crate::model::OutlierDetection,
    invalid: impl Fn(&'static str, &'static str) -> ConfigError,
) -> Result<(), ConfigError> {
    if !(110..=10_000).contains(&outlier.factor_percent) {
        return Err(invalid(
            "passive_health.outlier_detection.factor_percent",
            "factor must be between 110 and 10000 percent",
        ));
    }
    if !(1..=100_000).contains(&outlier.min_samples) {
        return Err(invalid(
            "passive_health.outlier_detection.min_samples",
            "minimum samples must be between 1 and 100000",
        ));
    }
    if !(100..=MAX_BREAKER_MS).contains(&outlier.interval_ms) {
        return Err(invalid(
            "passive_health.outlier_detection.interval_ms",
            "interval must be between 100 and 3600000 milliseconds",
        ));
    }
    if !(1..=100).contains(&outlier.max_ejected_percent) {
        return Err(invalid(
            "passive_health.outlier_detection.max_ejected_percent",
            "maximum ejected share must be between 1 and 100 percent",
        ));
    }
    Ok(())
}

pub(super) fn validate_circuit_breaker(
    breaker: &crate::model::CircuitBreaker,
    invalid: impl Fn(&'static str, &'static str) -> ConfigError,
) -> Result<(), ConfigError> {
    let field = |name| match name {
        "window_ms" => "passive_health.circuit_breaker.window_ms",
        "min_requests" => "passive_health.circuit_breaker.min_requests",
        "failure_ratio_percent" => "passive_health.circuit_breaker.failure_ratio_percent",
        "open_ms" => "passive_health.circuit_breaker.open_ms",
        "half_open_max" => "passive_health.circuit_breaker.half_open_max",
        _ => "passive_health.circuit_breaker.half_open_successes",
    };
    if !(1_000..=MAX_BREAKER_MS).contains(&breaker.window_ms) {
        return Err(invalid(
            field("window_ms"),
            "window must be between 1000 and 3600000 milliseconds",
        ));
    }
    if !(1..=100_000).contains(&breaker.min_requests) {
        return Err(invalid(
            field("min_requests"),
            "minimum requests must be between 1 and 100000",
        ));
    }
    if !(1..=100).contains(&breaker.failure_ratio_percent) {
        return Err(invalid(
            field("failure_ratio_percent"),
            "failure ratio must be between 1 and 100 percent",
        ));
    }
    if !(100..=MAX_BREAKER_MS).contains(&breaker.open_ms) {
        return Err(invalid(
            field("open_ms"),
            "open time must be between 100 and 3600000 milliseconds",
        ));
    }
    if !(1..=100).contains(&breaker.half_open_max) {
        return Err(invalid(
            field("half_open_max"),
            "half-open trial limit must be between 1 and 100",
        ));
    }
    if !(1..=100).contains(&breaker.half_open_successes) {
        return Err(invalid(
            field("half_open_successes"),
            "half-open successes must be between 1 and 100",
        ));
    }
    Ok(())
}

pub(super) fn validate_readmission(
    readmission: &crate::model::Readmission,
    invalid: impl Fn(&'static str, &'static str) -> ConfigError,
) -> Result<(), ConfigError> {
    if readmission.successes == 0 || readmission.successes > MAX_PASSIVE_RECOVERY_THRESHOLD {
        return Err(invalid(
            "passive_health.readmission.successes",
            "successes must be between 1 and 100",
        ));
    }
    if !(100..=MAX_READMISSION_INTERVAL_MS).contains(&readmission.interval_ms) {
        return Err(invalid(
            "passive_health.readmission.interval_ms",
            "interval must be between 100 and 3600000 milliseconds",
        ));
    }
    if !(10..=MAX_READMISSION_INTERVAL_MS).contains(&readmission.timeout_ms) {
        return Err(invalid(
            "passive_health.readmission.timeout_ms",
            "timeout must be between 10 and 3600000 milliseconds",
        ));
    }
    if let Some(probe) = &readmission.probe {
        if !probe.path.starts_with('/') {
            return Err(invalid(
                "passive_health.readmission.probe.path",
                "probe path must start with /",
            ));
        }
        if probe
            .expected_status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(invalid(
                "passive_health.readmission.probe.expected_status",
                "expected status must be between 100 and 599",
            ));
        }
    }
    Ok(())
}
