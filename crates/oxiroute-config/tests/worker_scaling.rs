use oxiroute_config::{ConfigDraft, ConfigError, ValidatedConfig, WorkerScaling};
use serde_json::{Value, json};

fn validate(extra: &Value) -> Result<ValidatedConfig, ConfigError> {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value::<ConfigDraft>(config)
        .expect("authored configuration")
        .validate()
}

fn dynamic(extra: Value) -> Value {
    let mut config = json!({
        "listener_workers": 8,
        "connection_scheduling": "aged_per_worker",
        "worker_scaling": "dynamic",
    });
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    config
}

#[test]
fn worker_scaling_is_static_by_default_and_stays_out_of_the_canonical_form() {
    let config = validate(&json!({})).unwrap();
    let draft = config.as_draft();

    assert_eq!(draft.worker_scaling, None);
    assert!(!draft.dynamic_worker_scaling());
    assert_eq!(draft.min_active_workers, None);
    assert_eq!(draft.target_busy_percent, None);
    assert_eq!(draft.scale_down_delay_ms, None);
    let canonical = serde_json::to_value(&config).unwrap();
    for key in [
        "worker_scaling",
        "min_active_workers",
        "target_busy_percent",
        "scale_down_delay_ms",
    ] {
        assert!(canonical.get(key).is_none(), "{key}");
    }
}

#[test]
fn static_scaling_decodes_and_is_not_dynamic() {
    let config = validate(&json!({"worker_scaling": "static"})).unwrap();

    assert_eq!(
        config.as_draft().worker_scaling,
        Some(WorkerScaling::Static)
    );
    assert!(!config.as_draft().dynamic_worker_scaling());
    assert_eq!(WorkerScaling::Static.as_str(), "static");
    assert_eq!(WorkerScaling::Dynamic.as_str(), "dynamic");
}

#[test]
fn dynamic_scaling_decodes_with_the_defaults() {
    let config = validate(&dynamic(json!({}))).unwrap();
    let draft = config.as_draft();

    assert_eq!(draft.worker_scaling, Some(WorkerScaling::Dynamic));
    assert!(draft.dynamic_worker_scaling());
    assert_eq!(draft.effective_min_active_workers(), 1);
    assert_eq!(draft.effective_target_busy_percent(), 50);
    assert_eq!(draft.effective_scale_down_delay_ms(), 5_000);
    assert_eq!(ConfigDraft::DEFAULT_MIN_ACTIVE_WORKERS, 1);
    assert_eq!(ConfigDraft::DEFAULT_TARGET_BUSY_PERCENT, 50);
    assert_eq!(ConfigDraft::DEFAULT_SCALE_DOWN_DELAY_MS, 5_000);
}

#[test]
fn every_tunable_decodes_and_overrides_its_default() {
    let config = validate(&dynamic(json!({
        "min_active_workers": 3,
        "target_busy_percent": 70,
        "scale_down_delay_ms": 250,
    })))
    .unwrap();
    let draft = config.as_draft();

    assert_eq!(draft.min_active_workers, Some(3));
    assert_eq!(draft.target_busy_percent, Some(70));
    assert_eq!(draft.scale_down_delay_ms, Some(250));
    assert_eq!(draft.effective_min_active_workers(), 3);
    assert_eq!(draft.effective_target_busy_percent(), 70);
    assert_eq!(draft.effective_scale_down_delay_ms(), 250);
}

#[test]
fn the_tunables_accept_their_bounds() {
    for (field, values) in [
        ("target_busy_percent", [10, 90]),
        ("scale_down_delay_ms", [100, 3_600_000]),
    ] {
        for value in values {
            validate(&dynamic(json!({field: value})))
                .unwrap_or_else(|error| panic!("{field} {value}: {error}"));
        }
    }
    validate(&dynamic(json!({"min_active_workers": 1}))).unwrap();
    validate(&dynamic(
        json!({"listener_workers": 256, "min_active_workers": 256}),
    ))
    .unwrap();
    assert_eq!(ConfigDraft::MAX_MIN_ACTIVE_WORKERS, 256);
    assert_eq!(ConfigDraft::MIN_TARGET_BUSY_PERCENT, 10);
    assert_eq!(ConfigDraft::MAX_TARGET_BUSY_PERCENT, 90);
    assert_eq!(ConfigDraft::MIN_SCALE_DOWN_DELAY_MS, 100);
    assert_eq!(ConfigDraft::MAX_SCALE_DOWN_DELAY_MS, 3_600_000);
}

#[test]
fn dynamic_scaling_needs_the_aged_per_worker_scheduling() {
    for scheduling in [json!({}), json!({"connection_scheduling": "work_stealing"})] {
        let mut config = json!({"listener_workers": 4, "worker_scaling": "dynamic"});
        config
            .as_object_mut()
            .unwrap()
            .extend(scheduling.as_object().unwrap().clone());
        let error = validate(&config).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::DynamicWorkerScalingWithoutAgedScheduling
            ),
            "{error}"
        );
        let message = error.to_string();
        assert!(
            message.contains("worker_scaling") && message.contains("aged_per_worker"),
            "{message}"
        );
    }
}

#[test]
fn the_tunables_need_dynamic_scaling() {
    let tunables = [
        ("min_active_workers", json!(1)),
        ("target_busy_percent", json!(50)),
        ("scale_down_delay_ms", json!(5_000)),
    ];
    for scaling in [json!({}), json!({"worker_scaling": "static"})] {
        for (field, value) in &tunables {
            let mut config =
                json!({"listener_workers": 4, "connection_scheduling": "aged_per_worker"});
            config
                .as_object_mut()
                .unwrap()
                .extend(scaling.as_object().unwrap().clone());
            config[field] = value.clone();
            let error = validate(&config).unwrap_err();
            assert!(
                matches!(error, ConfigError::WorkerScalingTunableWithoutDynamic(name) if name == *field),
                "{field}: {error}"
            );
            assert!(error.to_string().contains(field));
        }
    }
}

#[test]
fn min_active_workers_is_bounded() {
    for value in [0, ConfigDraft::MAX_MIN_ACTIVE_WORKERS + 1] {
        let error = validate(&dynamic(
            json!({"listener_workers": 256, "min_active_workers": value}),
        ))
        .unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidMinActiveWorkers(found) if found == value),
            "{error}"
        );
    }
}

#[test]
fn target_busy_percent_is_bounded() {
    for value in [0, 9, 91, 100] {
        let error = validate(&dynamic(json!({"target_busy_percent": value}))).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidTargetBusyPercent(found) if found == value),
            "{error}"
        );
    }
}

#[test]
fn scale_down_delay_is_bounded() {
    for value in [0, 99, 3_600_001] {
        let error = validate(&dynamic(json!({"scale_down_delay_ms": value}))).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidScaleDownDelay(found) if found == value),
            "{error}"
        );
    }
}

#[test]
fn min_active_workers_cannot_exceed_the_listener_workers() {
    let error = validate(&dynamic(
        json!({"listener_workers": 4, "min_active_workers": 5}),
    ))
    .unwrap_err();
    assert!(
        matches!(
            error,
            ConfigError::MinActiveWorkersAboveListenerWorkers {
                min_active: 5,
                workers: 4
            }
        ),
        "{error}"
    );
    validate(&dynamic(
        json!({"listener_workers": 4, "min_active_workers": 4}),
    ))
    .unwrap();
}

#[test]
fn absent_listener_workers_count_as_one() {
    let mut config = dynamic(json!({"min_active_workers": 2}));
    config.as_object_mut().unwrap().remove("listener_workers");
    let error = validate(&config).unwrap_err();
    assert!(
        matches!(
            error,
            ConfigError::MinActiveWorkersAboveListenerWorkers {
                min_active: 2,
                workers: 1
            }
        ),
        "{error}"
    );
    config["min_active_workers"] = json!(1);
    validate(&config).unwrap();
}

#[test]
fn an_unknown_worker_scaling_token_is_rejected_when_decoding() {
    let decoded = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1, "listeners": [], "worker_scaling": "elastic",
    }));
    assert!(decoded.is_err());
}
