use crate::fixtures::{minimal_config, normalized_round_trip, test_upstream_pools};
use oxiroute_config::{
    CircuitBreaker, ConfigError, OutlierDetection, PassiveHealthPolicy, PoolQueueClass,
    PoolQueuePolicy, PoolResilience, QueueDiscipline, Readmission, ReadmissionProbe,
};

#[test]
fn rejects_invalid_passive_health_bounds() {
    let mut pool = test_upstream_pools().remove(0);
    pool.passive_health = Some(PassiveHealthPolicy {
        error_limit: 0,
        ..PassiveHealthPolicy::default()
    });
    let mut config = minimal_config();
    config.upstream_pools.push(pool);

    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidUpstreamServer {
            field: "passive_health.error_limit",
            ..
        })
    ));
}

#[test]
fn rejects_invalid_readmission_bounds() {
    for (readmission, field) in [
        (
            Readmission {
                successes: 0,
                ..lenient_readmission()
            },
            "passive_health.readmission.successes",
        ),
        (
            Readmission {
                interval_ms: 5,
                ..lenient_readmission()
            },
            "passive_health.readmission.interval_ms",
        ),
        (
            Readmission {
                probe: Some(ReadmissionProbe {
                    path: "healthz".into(),
                    host: None,
                    expected_status: None,
                }),
                ..lenient_readmission()
            },
            "passive_health.readmission.probe.path",
        ),
    ] {
        let mut pool = test_upstream_pools().remove(0);
        pool.passive_health = Some(PassiveHealthPolicy {
            readmission: Some(readmission),
            ..PassiveHealthPolicy::default()
        });
        let mut config = minimal_config();
        config.upstream_pools.push(pool);

        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidUpstreamServer { field: found, .. }) if found == field
            ),
            "{field}"
        );
    }
}

#[test]
fn rejects_invalid_outlier_detection_bounds() {
    let valid = OutlierDetection {
        factor_percent: 300,
        min_samples: 50,
        interval_ms: 10_000,
        max_ejected_percent: 30,
    };
    for (outlier, field) in [
        (
            OutlierDetection {
                factor_percent: 100,
                ..valid
            },
            "passive_health.outlier_detection.factor_percent",
        ),
        (
            OutlierDetection {
                min_samples: 0,
                ..valid
            },
            "passive_health.outlier_detection.min_samples",
        ),
        (
            OutlierDetection {
                interval_ms: 50,
                ..valid
            },
            "passive_health.outlier_detection.interval_ms",
        ),
        (
            OutlierDetection {
                max_ejected_percent: 0,
                ..valid
            },
            "passive_health.outlier_detection.max_ejected_percent",
        ),
    ] {
        let mut pool = test_upstream_pools().remove(0);
        pool.passive_health = Some(PassiveHealthPolicy {
            outlier_detection: Some(outlier),
            ..PassiveHealthPolicy::default()
        });
        let mut config = minimal_config();
        config.upstream_pools.push(pool);

        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidUpstreamServer { field: found, .. }) if found == field
            ),
            "{field}"
        );
    }
}

#[test]
fn rejects_invalid_queue_policies() {
    let class = |name: &str, weight| PoolQueueClass {
        name: name.into(),
        weight,
    };
    let valid = PoolQueuePolicy {
        classes: vec![class("fast", 4), class("slow", 1)],
        discipline: QueueDiscipline::Weighted,
        class_header: Some("x-priority".into()),
        default_class: Some("slow".into()),
        tenant_header: Some("x-tenant".into()),
        max_tenants: 8,
        aging_ms: None,
    };
    for (queue, queue_timeout_ms, field) in [
        (valid.clone(), None, "queue"),
        (
            PoolQueuePolicy {
                classes: vec![class("fast", 4), class("fast", 1)],
                ..valid.clone()
            },
            Some(1_000),
            "queue.classes",
        ),
        (
            PoolQueuePolicy {
                classes: vec![class("Fast", 4)],
                ..valid.clone()
            },
            Some(1_000),
            "queue.classes",
        ),
        (
            PoolQueuePolicy {
                classes: vec![class("fast", 0)],
                ..valid.clone()
            },
            Some(1_000),
            "queue.classes",
        ),
        (
            PoolQueuePolicy {
                default_class: Some("gone".into()),
                ..valid.clone()
            },
            Some(1_000),
            "queue.default_class",
        ),
        (
            PoolQueuePolicy {
                class_header: Some("bad header".into()),
                ..valid.clone()
            },
            Some(1_000),
            "queue.class_header",
        ),
        (
            PoolQueuePolicy {
                max_tenants: 0,
                ..valid.clone()
            },
            Some(1_000),
            "queue.max_tenants",
        ),
        (
            PoolQueuePolicy {
                aging_ms: Some(1_000),
                ..valid.clone()
            },
            Some(1_000),
            "queue.aging_ms",
        ),
    ] {
        let mut pool = test_upstream_pools().remove(0);
        pool.queue_timeout_ms = queue_timeout_ms;
        pool.queue = Some(queue);
        let mut config = minimal_config();
        config.upstream_pools.push(pool);

        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidUpstreamServer { field: found, .. }) if found == field
            ),
            "{field}"
        );
    }
}

#[test]
fn rejects_invalid_circuit_breaker_bounds() {
    let valid = CircuitBreaker {
        window_ms: 10_000,
        min_requests: 10,
        failure_ratio_percent: 50,
        open_ms: 1_000,
        half_open_max: 1,
        half_open_successes: 2,
    };
    for (breaker, field) in [
        (
            CircuitBreaker {
                window_ms: 5,
                ..valid
            },
            "passive_health.circuit_breaker.window_ms",
        ),
        (
            CircuitBreaker {
                min_requests: 0,
                ..valid
            },
            "passive_health.circuit_breaker.min_requests",
        ),
        (
            CircuitBreaker {
                failure_ratio_percent: 101,
                ..valid
            },
            "passive_health.circuit_breaker.failure_ratio_percent",
        ),
        (
            CircuitBreaker {
                open_ms: 5,
                ..valid
            },
            "passive_health.circuit_breaker.open_ms",
        ),
        (
            CircuitBreaker {
                half_open_max: 0,
                ..valid
            },
            "passive_health.circuit_breaker.half_open_max",
        ),
        (
            CircuitBreaker {
                half_open_successes: 0,
                ..valid
            },
            "passive_health.circuit_breaker.half_open_successes",
        ),
    ] {
        let mut pool = test_upstream_pools().remove(0);
        pool.passive_health = Some(PassiveHealthPolicy {
            circuit_breaker: Some(breaker),
            ..PassiveHealthPolicy::default()
        });
        let mut config = minimal_config();
        config.upstream_pools.push(pool);

        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidUpstreamServer { field: found, .. }) if found == field
            ),
            "{field}"
        );
    }
}

fn lenient_readmission() -> Readmission {
    Readmission {
        successes: 2,
        interval_ms: 1_000,
        timeout_ms: 500,
        probe: None,
    }
}

#[test]
fn round_trips_pool_resilience_options() {
    let mut pool = test_upstream_pools().remove(0);
    pool.resilience = PoolResilience {
        fail_open: true,
        min_members: Some(1),
        ejection_jitter_percent: 25,
        queue_while_unavailable_ms: Some(3_000),
        unavailable_retry_after_seconds: Some(5),
        unavailable_body: Some("down for maintenance".into()),
    };
    let mut config = minimal_config();
    config.upstream_pools.push(pool.clone());

    let (normalized, source) = normalized_round_trip(config);

    assert_eq!(normalized.upstream_pools[0].resilience, pool.resilience);
    assert!(source.contains("fail_open = true"));
    assert!(source.contains("min_members = 1"));
    assert!(source.contains("ejection_jitter_percent = 25"));
    assert!(source.contains("queue_while_unavailable_ms = 3000"));
    assert!(source.contains("unavailable_retry_after_seconds = 5"));
    assert!(source.contains("unavailable_body = \"down for maintenance\""));
}

#[test]
fn rejects_invalid_pool_resilience_bounds() {
    for (resilience, field) in [
        (
            PoolResilience {
                min_members: Some(99),
                ..PoolResilience::default()
            },
            "resilience.min_members",
        ),
        (
            PoolResilience {
                ejection_jitter_percent: 51,
                ..PoolResilience::default()
            },
            "resilience.ejection_jitter_percent",
        ),
        (
            PoolResilience {
                queue_while_unavailable_ms: Some(86_400_001),
                ..PoolResilience::default()
            },
            "resilience.queue_while_unavailable_ms",
        ),
        (
            PoolResilience {
                unavailable_retry_after_seconds: Some(0),
                ..PoolResilience::default()
            },
            "resilience.unavailable_retry_after_seconds",
        ),
        (
            PoolResilience {
                unavailable_body: Some(String::new()),
                ..PoolResilience::default()
            },
            "resilience.unavailable_body",
        ),
    ] {
        let mut pool = test_upstream_pools().remove(0);
        pool.resilience = resilience;
        let mut config = minimal_config();
        config.upstream_pools.push(pool);

        assert!(
            matches!(
                config.validate(),
                Err(ConfigError::InvalidUpstreamServer { field: invalid, .. }) if invalid == field
            ),
            "{field}"
        );
    }
}
