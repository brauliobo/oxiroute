use oxiroute_config::{ConfigDraft, ConfigError, ConnectionScheduling};
use serde_json::{Value, json};

fn validate(extra: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value::<ConfigDraft>(config)
        .expect("authored configuration")
        .validate()
}

#[test]
fn connection_scheduling_is_absent_by_default_and_stays_out_of_the_canonical_form() {
    let config = validate(&json!({})).unwrap();

    assert_eq!(config.as_draft().connection_scheduling, None);
    assert_eq!(config.as_draft().connection_age_ms, None);
    let canonical = serde_json::to_value(&config).unwrap();
    assert!(canonical.get("connection_scheduling").is_none());
    assert!(canonical.get("connection_age_ms").is_none());
}

#[test]
fn the_aged_per_worker_mode_takes_the_default_age_or_a_configured_one() {
    let config = validate(&json!({"connection_scheduling": "aged_per_worker"})).unwrap();
    assert_eq!(
        config.as_draft().connection_scheduling,
        Some(ConnectionScheduling::AgedPerWorker)
    );
    assert_eq!(
        config.as_draft().effective_connection_age_ms(),
        Some(ConfigDraft::DEFAULT_CONNECTION_AGE_MS)
    );

    for age in [1, 100, ConfigDraft::MAX_CONNECTION_AGE_MS] {
        let config = validate(&json!({
            "connection_scheduling": "aged_per_worker",
            "connection_age_ms": age,
        }))
        .unwrap();
        assert_eq!(config.as_draft().effective_connection_age_ms(), Some(age));
    }
}

#[test]
fn work_stealing_has_no_age() {
    for scheduling in [json!({}), json!({"connection_scheduling": "work_stealing"})] {
        assert_eq!(
            validate(&scheduling)
                .unwrap()
                .as_draft()
                .effective_connection_age_ms(),
            None
        );
    }
}

#[test]
fn an_age_needs_the_aged_per_worker_mode() {
    for scheduling in [json!({}), json!({"connection_scheduling": "work_stealing"})] {
        let mut config = scheduling.clone();
        config["connection_age_ms"] = json!(40);
        let error = validate(&config).unwrap_err();
        assert!(
            matches!(error, ConfigError::ConnectionAgeWithoutAgedScheduling),
            "{error}"
        );
    }
}

#[test]
fn the_age_is_bounded() {
    for age in [0, ConfigDraft::MAX_CONNECTION_AGE_MS + 1] {
        let error = validate(&json!({
            "connection_scheduling": "aged_per_worker",
            "connection_age_ms": age,
        }))
        .unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidConnectionAge(value) if value == age),
            "{error}"
        );
    }
}
